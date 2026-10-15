//! Deliberately bounded SQL subset. The engine still decides validity/permissions.
use crate::policy::Risk;
use sqlparser::{
    ast::{Expr, Query, Select, SetExpr, Statement, TableFactor, Visit, Visitor},
    dialect::{
        Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect,
    },
    parser::Parser,
    tokenizer::Tokenizer,
};
use std::ops::ControlFlow;

pub(crate) const SQL_LIMIT: usize = 64 * 1024;
pub(crate) fn function(name: &str) -> bool {
    [
        "abs",
        "avg",
        "count",
        "sum",
        "min",
        "max",
        "total",
        "round",
        "lower",
        "upper",
        "length",
        "char_length",
        "coalesce",
        "ifnull",
        "nullif",
        "trim",
        "ltrim",
        "rtrim",
        "substr",
        "substring",
        "replace",
        "instr",
        "concat",
        "concat_ws",
        "date",
        "time",
        "datetime",
        "strftime",
        "julianday",
        "unixepoch",
        "date_part",
        "date_trunc",
        "extract",
        "json_extract",
        "json_valid",
        "json_type",
        "json_array_length",
        "row_number",
        "rank",
        "dense_rank",
        "lag",
        "lead",
        "first_value",
        "last_value",
        "group_concat",
        "string_agg",
        "printf",
        "typeof",
        "hex",
        "unicode",
        "iif",
        "like",
        "glob",
        "random",
        "randomblob",
        "zeroblob",
        "changes",
        "last_insert_rowid",
    ]
    .contains(&name.to_ascii_lowercase().as_str())
}
fn body(q: &SetExpr, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match q {
        SetExpr::Select(_) | SetExpr::Query(_) | SetExpr::Values(_) => true,
        SetExpr::SetOperation { left, right, .. } => {
            body(left, depth + 1) && body(right, depth + 1)
        }
        _ => false,
    }
}
struct Guard {
    nodes: usize,
    first: bool,
    depth: usize,
}
impl Guard {
    fn node(&mut self) -> ControlFlow<&'static str> {
        self.nodes += 1;
        if self.nodes > 4096 {
            ControlFlow::Break("DATABASE_SQL_COMPLEXITY")
        } else {
            ControlFlow::Continue(())
        }
    }
}
impl Visitor for Guard {
    type Break = &'static str;
    fn pre_visit_query(&mut self, q: &Query) -> ControlFlow<Self::Break> {
        self.node()?;
        self.depth += 1;
        if self.depth > 32 {
            return ControlFlow::Break("DATABASE_SQL_COMPLEXITY");
        }
        if !body(&q.body, 0)
            || !q.locks.is_empty()
            || q.settings.is_some()
            || q.format_clause.is_some()
            || !q.pipe_operators.is_empty()
        {
            return ControlFlow::Break("DATABASE_QUERY_EFFECT_DENIED");
        }
        ControlFlow::Continue(())
    }
    fn post_visit_query(&mut self, _: &Query) -> ControlFlow<Self::Break> {
        self.depth -= 1;
        ControlFlow::Continue(())
    }
    fn pre_visit_select(&mut self, s: &Select) -> ControlFlow<Self::Break> {
        self.node()?;
        if s.into.is_some() {
            return ControlFlow::Break("DATABASE_SELECT_INTO_DENIED");
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_relation(
        &mut self,
        name: &sqlparser::ast::ObjectName,
    ) -> ControlFlow<Self::Break> {
        self.node()?;
        if name.0.len() > 2 {
            return ControlFlow::Break("DATABASE_CROSS_CATALOG_DENIED");
        }
        ControlFlow::Continue(())
    }
    fn pre_visit_table_factor(&mut self, t: &TableFactor) -> ControlFlow<Self::Break> {
        self.node()?;
        match t {
            TableFactor::Table { args: None, .. }
            | TableFactor::Derived { .. }
            | TableFactor::NestedJoin { .. } => ControlFlow::Continue(()),
            _ => ControlFlow::Break("DATABASE_TABLE_FUNCTION_DENIED"),
        }
    }
    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<Self::Break> {
        self.node()?;
        self.depth += 1;
        if self.depth > 32 {
            return ControlFlow::Break("DATABASE_SQL_COMPLEXITY");
        }
        match e {
            Expr::Function(f) if !function(&f.name.to_string()) => {
                ControlFlow::Break("DATABASE_FUNCTION_DENIED")
            }
            // User-defined operators/casts can run provider code on remote servers;
            // least-privilege server credentials remain required, even for a view.
            Expr::Function(_) => ControlFlow::Continue(()),
            _ => ControlFlow::Continue(()),
        }
    }
    fn post_visit_expr(&mut self, _: &Expr) -> ControlFlow<Self::Break> {
        self.depth -= 1;
        ControlFlow::Continue(())
    }
    fn pre_visit_statement(&mut self, s: &Statement) -> ControlFlow<Self::Break> {
        self.node()?;
        if self.first {
            self.first = false;
            return ControlFlow::Continue(());
        }
        if matches!(s, Statement::Query(_)) {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break("DATABASE_NESTED_WRITE_DENIED")
        }
    }
}
fn scoped_delete(e: &Expr) -> bool {
    use sqlparser::ast::BinaryOperator;
    match e {
        Expr::Nested(e) => scoped_delete(e),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => scoped_delete(left) && scoped_delete(right),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } => {
            matches!(
                left.as_ref(),
                Expr::Identifier(_) | Expr::CompoundIdentifier(_)
            ) && matches!(right.as_ref(), Expr::Value(_))
        }
        _ => false,
    }
}
pub(crate) fn analyze(engine: &str, sql: &str) -> Result<Risk, String> {
    if sql.is_empty() || sql.len() > SQL_LIMIT || sql.contains('\0') {
        return Err("DATABASE_SQL_LENGTH".into());
    }
    // Reject executable MySQL comments and hints before tokenization; the parser
    // treats comments as inert while the server can execute their contents.
    if sql.contains("/*!") || sql.contains("/*M!") || sql.contains("/*+") {
        return Err("DATABASE_EXECUTABLE_COMMENT_DENIED".into());
    }
    let dialect: Box<dyn Dialect> = match engine {
        "sqlite" => Box::new(SQLiteDialect {}),
        "mysql" | "mariadb" => Box::new(MySqlDialect {}),
        "postgresql" => Box::new(PostgreSqlDialect {}),
        "sqlserver" => Box::new(MsSqlDialect {}),
        _ => Box::new(GenericDialect {}),
    };
    // Bound flat operator/UNION chains before building a recursively dropped AST.
    if Tokenizer::new(dialect.as_ref(), sql)
        .tokenize()
        .map_err(|_| "DATABASE_SQL_PARSE")?
        .len()
        > 1024
    {
        return Err("DATABASE_SQL_COMPLEXITY".into());
    }
    let statements = Parser::new(dialect.as_ref())
        .with_recursion_limit(32)
        .try_with_sql(sql)
        .map_err(|_| "DATABASE_SQL_PARSE")?
        .parse_statements()
        .map_err(|_| "DATABASE_SQL_PARSE")?;
    if statements.len() != 1 {
        return Err("DATABASE_SINGLE_STATEMENT_REQUIRED".into());
    }
    let statement = &statements[0];
    let risk = match statement {
        Statement::Query(_) => Risk::Free,
        Statement::Insert(_) | Statement::Update(_) => Risk::Contextual,
        Statement::Delete(d) if d.selection.as_ref().is_some_and(scoped_delete) => Risk::Contextual,
        Statement::Delete(_)
        | Statement::CreateTable(_)
        | Statement::AlterTable { .. }
        | Statement::Drop { .. }
        | Statement::Truncate { .. } => Risk::Mandatory,
        _ => return Err("DATABASE_STATEMENT_DENIED".into()),
    };
    let mut guard = Guard {
        nodes: 0,
        first: true,
        depth: 0,
    };
    if let ControlFlow::Break(error) = statement.visit(&mut guard) {
        return Err(error.into());
    }
    Ok(risk)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ast_effects_and_dialects() {
        for engine in ["sqlite", "mysql", "postgresql", "sqlserver"] {
            assert_eq!(
                analyze(engine, "WITH a AS (SELECT 1 AS n) SELECT count(*) FROM a").unwrap(),
                Risk::Free
            );
            assert_eq!(
                analyze(engine, "UPDATE items SET n=2 WHERE id=1").unwrap(),
                Risk::Contextual
            );
            assert_eq!(
                analyze(engine, "DELETE FROM items WHERE id=1").unwrap(),
                Risk::Contextual
            );
            assert_eq!(
                analyze(engine, "DELETE FROM items WHERE 1=1").unwrap(),
                Risk::Mandatory
            );
            for sql in [
                "SELECT 1; DELETE FROM items",
                "SELECT load_extension('x')",
                "CALL proc()",
                "BEGIN",
                "ATTACH 'x' AS x",
                "SELECT writefile('x','y')",
                "SELECT custom_side_effect()",
            ] {
                assert!(analyze(engine, sql).is_err(), "{engine}: {sql}");
            }
        }
        for (engine, sql) in [
            (
                "postgresql",
                "WITH x AS (DELETE FROM items RETURNING *) SELECT * FROM x",
            ),
            ("sqlserver", "SELECT * INTO copy FROM items"),
            ("mysql", "SELECT 1 INTO OUTFILE '/tmp/x'"),
            ("mysql", "SELECT 1 /*!50000 INTO OUTFILE '/tmp/x' */"),
            ("postgresql", "SELECT * FROM items FOR UPDATE"),
        ] {
            assert!(analyze(engine, sql).is_err(), "{engine}: {sql}");
        }
        assert!(analyze("sqlite", &format!("SELECT {}", vec!["1"; 1000].join("+"))).is_err());
    }
}
