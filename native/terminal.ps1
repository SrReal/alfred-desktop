param([ValidateSet('analyze','execute')][string]$Mode='execute')
$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$request=[Console]::In.ReadToEnd() | ConvertFrom-Json
if($Mode -eq 'analyze'){
  $tokens=$null;$parseErrors=$null
  $tree=[Management.Automation.Language.Parser]::ParseInput([string]$request.command,[ref]$tokens,[ref]$parseErrors)
  $commands=@($tree.FindAll({param($node) $node -is [Management.Automation.Language.CommandAst]},$true) | ForEach-Object {$_.GetCommandName()})
  # Only literal pipelines can qualify for the backend's small read-only command list.
  $complex=@($tree.FindAll({param($node)
    $node.GetType().Name -notin @('ScriptBlockAst','NamedBlockAst','PipelineAst','CommandAst','StringConstantExpressionAst','ConstantExpressionAst','CommandParameterAst') -or
    ($node -is [Management.Automation.Language.CommandAst] -and $node.InvocationOperator -ne [Management.Automation.Language.TokenKind]::Unknown)
  },$true)).Count -gt 0
  @{valid=($parseErrors.Count -eq 0);commands=$commands;complex=$complex;errors=@($parseErrors | ForEach-Object {$_.ErrorId})}|ConvertTo-Json -Compress
  exit 0
}
try {
  Set-Location -LiteralPath ([string]$request.cwd)
  & ([ScriptBlock]::Create([string]$request.command))
  if($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0){exit $LASTEXITCODE}
  exit 0
} catch {
  [Console]::Error.WriteLine($_.Exception.Message)
  exit 1
}
