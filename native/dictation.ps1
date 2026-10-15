param([ValidateSet('info','listen')][string]$Mode='info')
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)
try {
  Add-Type -AssemblyName System.Speech
  Add-Type -AssemblyName System.Web.Extensions
  Add-Type -Path (Join-Path $PSScriptRoot 'dictation.cs') -ReferencedAssemblies @([System.Speech.Recognition.SpeechRecognitionEngine].Assembly.Location,[System.Web.Script.Serialization.JavaScriptSerializer].Assembly.Location,'System.Core')
  if($Mode -eq 'info'){[AlfredPet.Dictation.Worker]::Info()}else{[AlfredPet.Dictation.Worker]::Run()}
} catch {
  [Console]::WriteLine('{"type":"error","code":"DICTATION_ENGINE_UNAVAILABLE"}')
  exit 1
}
