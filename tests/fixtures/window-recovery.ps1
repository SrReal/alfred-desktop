param([string]$MetadataPath,[string]$CommandPath)
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Windows.Forms,System.Drawing,System.Web.Extensions
Add-Type -ReferencedAssemblies System.Windows.Forms,System.Drawing,System.Web.Extensions -TypeDefinition ([System.IO.File]::ReadAllText((Join-Path $PSScriptRoot 'window-recovery.cs')))
[RecoveryFixture]::Run($MetadataPath,$CommandPath)
