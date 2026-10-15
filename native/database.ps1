param()
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$envelope = [Console]::In.ReadToEnd() | ConvertFrom-Json
$p = $envelope.input
$connection = $null
$transaction = $null
$committed = $false
$writeStarted = $false
$clock = [Diagnostics.Stopwatch]::StartNew()

function Check-Time {
    if ($clock.Elapsed.TotalSeconds -gt 45) { throw 'DATABASE_TIMEOUT' }
}
function Connection-Options($profile) {
    $b = [Data.Odbc.OdbcConnectionStringBuilder]::new()
    $b['Driver'] = [string]$profile.driver
    $b['Database'] = [string]$profile.database
    $b['Uid'] = [string]$profile.user
    $b['Pwd'] = [string]$profile.secret
    switch ([string]$profile.engine) {
        'sqlserver' {
            if ($profile.driver -ne 'ODBC Driver 18 for SQL Server') { throw 'DATABASE_DRIVER_UNSUPPORTED' }
            $hostName=[string]$profile.host
            if ($hostName.Contains(':')) { $hostName='['+$hostName+']' }
            $b['Server'] = 'tcp:' + $hostName + ',' + [string]$profile.port
            $b['Encrypt'] = $(if ($profile.ssl -eq 'disable') { 'no' } else { 'yes' })
            $b['TrustServerCertificate'] = $(if ($profile.ssl -eq 'require') { 'yes' } else { 'no' })
            $b['ConnectTimeout'] = '10'
        }
        'postgresql' {
            if ($profile.driver -notin @('PostgreSQL Unicode(x64)', 'PostgreSQL Unicode')) { throw 'DATABASE_DRIVER_UNSUPPORTED' }
            $b['Servername'] = [string]$profile.host
            $b['Port'] = [string]$profile.port
            $b['SSLmode'] = $(switch ($profile.ssl) { 'verify_full' { 'verify-full' } 'require' { 'require' } 'disable' { 'disable' } default { throw 'DATABASE_SSL_REQUIRED' } })
            $b['ReadOnly'] = $(if ($profile.write) { '0' } else { '1' })
            $b['Debug'] = '0'; $b['CommLog'] = '0'; $b['UseDeclareFetch'] = '1'; $b['Fetch'] = '100'; $b['IgnoreTimeout'] = '0'
            $pq = 'connect_timeout=10'
            if ($profile.ssl -eq 'verify_full') {
                # libpq conninfo quoting is independent of ODBC brace quoting.
                $ca = ([string]$profile.ca_path).Replace('\', '\\').Replace("'", "\'")
                $pq += " sslrootcert='$ca'"
            }
            $b['Pqopt'] = $pq
        }
        'mysql' {
            if ([string]$profile.driver -notmatch '^MySQL ODBC (8\.[0-9.]+|9\.[0-9.]+|[12][0-9]\.[0-9.]+) Unicode Driver$') { throw 'DATABASE_DRIVER_UNSUPPORTED' }
            $b['Server'] = [string]$profile.host; $b['Port'] = [string]$profile.port
            $b['SSLMODE'] = $(switch ($profile.ssl) { 'verify_full' { 'VERIFY_IDENTITY' } 'require' { 'REQUIRED' } 'disable' { 'DISABLED' } default { throw 'DATABASE_SSL_REQUIRED' } })
            if ($profile.ssl -eq 'verify_full') { $b['SSLCA'] = [string]$profile.ca_path }
            $b['USE_MYCNF'] = '0'; $b['LOG_QUERY'] = '0'; $b['AUTO_RECONNECT'] = '0'; $b['MULTI_STATEMENTS'] = '0'
            $b['NO_PROMPT'] = '1'; $b['NO_CACHE'] = '1'; $b['FORWARD_CURSOR'] = '1'; $b['CONNECT_TIMEOUT'] = '10'
        }
        'mariadb' {
            if ($profile.driver -notin @('MariaDB ODBC 3.1 Driver', 'MariaDB ODBC 3.2 Driver')) { throw 'DATABASE_DRIVER_UNSUPPORTED' }
            if ($profile.ssl -eq 'disable') { throw 'DATABASE_MARIADB_TLS_DISABLE_UNSUPPORTED' }
            $b['Server'] = [string]$profile.host; $b['Port'] = [string]$profile.port
            $b['FORCETLS'] = '1'; $b['SSLVERIFY'] = $(if ($profile.ssl -eq 'verify_full') { '1' } else { '0' })
            if ($profile.ssl -eq 'verify_full') { $b['SSLCA'] = [string]$profile.ca_path }
            $b['CONN_TIMEOUT'] = '10'; $b['AUTO_RECONNECT'] = '0'; $b['NO_PROMPT'] = '1'
        }
        default { throw 'DATABASE_ENGINE_UNSUPPORTED' }
    }
    return $b
}
function Command($sql, $parameters = @()) {
    Check-Time
    $cmd = $connection.CreateCommand()
    $cmd.CommandTimeout = 20
    $cmd.CommandText = $sql
    if ($transaction) { $cmd.Transaction = $transaction }
    foreach ($v in $parameters) {
        $param = [Data.Odbc.OdbcParameter]::new()
        if ($null -eq $v) { $param.OdbcType = [Data.Odbc.OdbcType]::NVarChar; $param.Value = [DBNull]::Value }
        elseif ($v -is [bool]) { $param.OdbcType = [Data.Odbc.OdbcType]::Bit; $param.Value = $v }
        elseif ($v -is [string]) { $param.OdbcType = [Data.Odbc.OdbcType]::NVarChar; $param.Value = $v; $param.Size = [Math]::Max(1,$v.Length) }
        elseif ($v -is [int] -or $v -is [long]) { $param.OdbcType = [Data.Odbc.OdbcType]::BigInt; $param.Value = [long]$v }
        elseif ($v -is [double] -or $v -is [decimal]) { $param.OdbcType = [Data.Odbc.OdbcType]::Double; $param.Value = [double]$v }
        elseif ($v.PSObject.Properties.Name -contains 'blob_base64') { $param.OdbcType = [Data.Odbc.OdbcType]::VarBinary; $param.Value = [Convert]::FromBase64String([string]$v.blob_base64); $param.Size = [Math]::Max(1,$param.Value.Length) }
        else { $cmd.Dispose(); throw 'DATABASE_PARAMETER_TYPE' }
        [void]$cmd.Parameters.Add($param)
    }
    return $cmd
}
function NonQuery($sql) {
    $cmd = Command $sql
    try { [void]$cmd.ExecuteNonQuery() } finally { $cmd.Dispose() }
}
function Read-Rows($sql, $parameters = @(), $limit = 5000) {
    $cmd = Command $sql $parameters
    $reader = $null
    try {
        $reader = $cmd.ExecuteReader([Data.CommandBehavior]::SequentialAccess)
        if ($reader.FieldCount -gt 128) { throw 'DATABASE_COLUMN_LIMIT' }
        $columns = @(); for ($i=0;$i -lt $reader.FieldCount;$i++) { $columns += [string]$reader.GetName($i) }
        $rows = [Collections.Generic.List[object]]::new(); $size = 0; $incomplete = $false
        while ($reader.Read()) {
            Check-Time
            if ($rows.Count -eq $limit) { $incomplete = $true; break }
            $values = [Collections.Generic.List[object]]::new()
            for ($i=0;$i -lt $reader.FieldCount;$i++) {
                if ($reader.IsDBNull($i)) { $values.Add($null); continue }
                $type = $reader.GetFieldType($i)
                if ($type -eq [string]) {
                    $n = $reader.GetChars($i,0,$null,0,0)
                    if ($n -gt 65536) { throw 'DATABASE_CELL_LIMIT' }
                    $chars = [char[]]::new([int]$n); if ($n) { [void]$reader.GetChars($i,0,$chars,0,[int]$n) }
                    $text = [string]::new($chars)
                    if ([Text.Encoding]::UTF8.GetByteCount($text) -gt 65536) { throw 'DATABASE_CELL_LIMIT' }
                    $values.Add($text)
                } elseif ($type -eq [byte[]]) {
                    $n = $reader.GetBytes($i,0,$null,0,0)
                    if ($n -gt 65536) { throw 'DATABASE_CELL_LIMIT' }
                    $bytes = [byte[]]::new([int]$n); if ($n) { [void]$reader.GetBytes($i,0,$bytes,0,[int]$n) }
                    $values.Add(@{blob_base64=[Convert]::ToBase64String($bytes)})
                } else {
                    $v = $reader.GetValue($i)
                    if ($v -is [decimal]) { $v = $v.ToString([Globalization.CultureInfo]::InvariantCulture) }
                    elseif ($v -is [DateTime]) { $v = $v.ToString('o',[Globalization.CultureInfo]::InvariantCulture) }
                    elseif ($v -is [Guid] -or $v -is [TimeSpan]) { $v = $v.ToString() }
                    elseif ($v -isnot [bool] -and $v -isnot [byte] -and $v -isnot [short] -and $v -isnot [int] -and $v -isnot [long] -and $v -isnot [float] -and $v -isnot [double]) { throw 'DATABASE_CELL_TYPE' }
                    if (($v -is [double] -or $v -is [float]) -and ([double]::IsNaN($v) -or [double]::IsInfinity($v))) { throw 'DATABASE_NUMBER' }
                    $values.Add($v)
                }
            }
            $row = $values.ToArray()
            $size += [Text.Encoding]::UTF8.GetByteCount((ConvertTo-Json -InputObject $row -Depth 8 -Compress))
            if ($size -gt 4128768) { throw 'DATABASE_OUTPUT_LIMIT' }
            $rows.Add($row)
        }
        return [ordered]@{columns=$columns;format='alfredpet.database.rows.v1';incomplete=$incomplete;rows=$rows.ToArray()}
    } finally { if ($reader) { $reader.Dispose() }; $cmd.Dispose() }
}
function Schema {
    $filter = switch ($p.engine) { 'mysql' { 'TABLE_SCHEMA=DATABASE()' } 'mariadb' { 'TABLE_SCHEMA=DATABASE()' } 'postgresql' { "TABLE_CATALOG=current_database() AND TABLE_SCHEMA NOT IN ('pg_catalog','information_schema')" } 'sqlserver' { 'TABLE_CATALOG=DB_NAME()' } }
    $objects = Read-Rows "SELECT TABLE_SCHEMA AS table_schema,TABLE_NAME AS table_name,TABLE_TYPE AS table_type FROM INFORMATION_SCHEMA.TABLES WHERE $filter ORDER BY TABLE_SCHEMA,TABLE_NAME"
    $columns = Read-Rows "SELECT TABLE_SCHEMA AS table_schema,TABLE_NAME AS table_name,COLUMN_NAME AS column_name,DATA_TYPE AS data_type,IS_NULLABLE AS nullable,COLUMN_DEFAULT AS column_default FROM INFORMATION_SCHEMA.COLUMNS WHERE $filter ORDER BY TABLE_SCHEMA,TABLE_NAME,ORDINAL_POSITION"
    if ($objects.incomplete -or $columns.incomplete) { throw 'DATABASE_SCHEMA_LIMIT' }
    return [ordered]@{columns=$columns;objects=$objects}
}
function Canonical($value) {
    if ($null -eq $value -or $value -is [string] -or $value -is [ValueType]) { return $value }
    if ($value -is [Collections.IDictionary]) {
        $result = [ordered]@{}; foreach ($key in @($value.Keys | Sort-Object -CaseSensitive)) { $result[$key] = Canonical $value[$key] }; return $result
    }
    if ($value -is [Collections.IEnumerable]) { $list=[Collections.Generic.List[object]]::new(); foreach ($v in $value) { $list.Add((Canonical $v)) }; return ,$list.ToArray() }
    $result = [ordered]@{}; foreach ($prop in @($value.PSObject.Properties | Sort-Object Name -CaseSensitive)) { $result[$prop.Name] = Canonical $prop.Value }; return $result
}
function Quote-Name($s) {
    if ($s -notmatch '^[A-Za-z_][A-Za-z0-9_]{0,127}$') { throw 'DATABASE_IDENTIFIER' }
    switch ($p.engine) { 'mysql' { return '`'+$s+'`' } 'mariadb' { return '`'+$s+'`' } 'sqlserver' { return '['+$s+']' } default { return '"'+$s+'"' } }
}
try {
    if ($envelope.operation -ne 'database.odbc') { throw 'DATABASE_OPERATION' }
    if ($p.read_only -ne $false -and $p.write) { throw 'DATABASE_READ_ONLY' }
    $builder = Connection-Options $p
    # Tests dot-source only the functions; no production dry-run or server bypass.
    $drivers = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SOFTWARE\ODBC\ODBCINST.INI\ODBC Drivers')
    try { if (!$drivers -or $drivers.GetValue([string]$p.driver) -ne 'Installed') { throw 'DATABASE_DRIVER_NOT_INSTALLED' } } finally { if ($drivers) { $drivers.Dispose() } }
    $connection = [Data.Odbc.OdbcConnection]::new($builder.ConnectionString)
    $connection.Open(); Check-Time
    if (!$p.write) {
        if ($p.engine -eq 'postgresql') { NonQuery 'SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY' }
        elseif ($p.engine -in @('mysql','mariadb')) { NonQuery 'SET SESSION TRANSACTION READ ONLY' }
    }
    $transaction = $connection.BeginTransaction([Data.IsolationLevel]::Serializable)
    $meta = Schema
    if ($p.action -eq 'schema') { $result = $meta }
    else {
        $before = ConvertTo-Json -InputObject (Canonical $meta) -Depth 20 -Compress
        $expected = ConvertTo-Json -InputObject (Canonical $p.request.expected_schema) -Depth 20 -Compress
        if ($before -cne $expected) { throw 'DATABASE_SCHEMA_OBSERVATION_CHANGED' }
        switch ($p.action) {
            { $_ -in @('query','export','explain') } {
                $sql = [string]$p.request.sql; $plan = $false
                try {
                    if ($p.action -eq 'explain') {
                        switch ($p.engine) { 'postgresql' { $sql='EXPLAIN (FORMAT JSON, ANALYZE FALSE) '+$sql } 'sqlserver' { NonQuery 'SET SHOWPLAN_XML ON'; $plan=$true } default { $sql='EXPLAIN FORMAT=JSON '+$sql } }
                    }
                    $limit = if ($p.request.limit) { [int]$p.request.limit } else { 500 }
                    $arguments = if ($null -eq $p.request.parameters) { @() } else { @($p.request.parameters) }
                    $result = Read-Rows $sql $arguments $limit
                } finally { if ($plan) { NonQuery 'SET SHOWPLAN_XML OFF' } }
            }
            'execute' {
                if (!$p.write) { throw 'DATABASE_READ_ONLY' }
                $arguments = if ($null -eq $p.request.parameters) { @() } else { @($p.request.parameters) }
                $cmd = Command ([string]$p.request.sql) $arguments
                try { $writeStarted=$true; $changed=$cmd.ExecuteNonQuery(); $result=@{changed=$changed;transaction='committed';remote_ddl_atomic=$false} } finally { $cmd.Dispose() }
            }
            'import' {
                if (!$p.write) { throw 'DATABASE_READ_ONLY' }
                $d=$p.request.data
                if ($d.format -ne 'alfredpet.database.rows.v1' -or $d.incomplete -or $d.rows.Count -gt 5000 -or $d.columns.Count -gt 128) { throw 'DATABASE_IMPORT_FORMAT' }
                $names = @($d.columns | ForEach-Object { Quote-Name ([string]$_) }) -join ','
                $marks = (@($d.columns | ForEach-Object { '?' }) -join ',')
                $sql = 'INSERT INTO '+(Quote-Name ([string]$p.request.table))+" ($names) VALUES ($marks)"
                $changed=0
                foreach ($row in $d.rows) {
                    Check-Time
                    $cmd = Command $sql @($row)
                    try { $writeStarted=$true; $changed += $cmd.ExecuteNonQuery() } finally { $cmd.Dispose() }
                }
                $result=@{changed=$changed;transaction='committed';remote_ddl_atomic=$false}
            }
            default { throw 'DATABASE_ACTION' }
        }
    }
    Check-Time
    if ($p.write) { $transaction.Commit(); $committed=$true } else { $transaction.Rollback(); $committed=$true }
    @{ok=$true;data=$result} | ConvertTo-Json -Depth 20 -Compress | Write-Output
} catch {
    # Never return driver exception text: it can include SQL, values or credentials.
    $errorCode = $(if ($p.write -and $writeStarted) { 'DATABASE_WRITE_OUTCOME_UNCERTAIN' } else { 'DATABASE_NATIVE_FAILED' })
    if (!$writeStarted -and $_.Exception.Message -cmatch '^DATABASE_[A-Z_]+$') { $errorCode=$_.Exception.Message }
    @{ok=$false;error=$errorCode} | ConvertTo-Json -Compress | Write-Output
    exit 1
} finally {
    if ($transaction) { if (!$committed) { try { $transaction.Rollback() } catch {} }; $transaction.Dispose() }
    if ($connection) { $connection.Dispose() }
}
