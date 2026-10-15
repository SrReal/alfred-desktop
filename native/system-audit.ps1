$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
$sections=[Collections.Generic.List[object]]::new()
function Section([string]$name,[scriptblock]$read) {
  try { $data=& $read; $sections.Add(@{name=$name;status='observed';data=$data}) }
  catch { $sections.Add(@{name=$name;status='unavailable';error=$_.FullyQualifiedErrorId}) }
}
Section 'Protección antivirus registrada' {
  @(Get-CimInstance -Namespace root/SecurityCenter2 -ClassName AntiVirusProduct -ErrorAction Stop |
    Select-Object displayName,productState)
}
Section 'Microsoft Defender' {
  Get-MpComputerStatus -ErrorAction Stop | Select-Object AMServiceEnabled,AntivirusEnabled,AntispywareEnabled,
    RealTimeProtectionEnabled,BehaviorMonitorEnabled,IsTamperProtected,AMRunningMode,
    AntivirusSignatureLastUpdated,AntivirusSignatureAge,QuickScanStartTime,QuickScanEndTime,FullScanStartTime,FullScanEndTime
}
Section 'Amenazas detectadas por Defender' {
  @(Get-MpThreat -ErrorAction Stop | Select-Object -First 50 ThreatID,ThreatName,IsActive,DidThreatExecute,SeverityID)
}
Section 'Errores de Windows en las últimas 24 horas' {
  $items=[Collections.Generic.List[object]]::new()
  foreach($log in @('System','Application')) {
    try {
      foreach($item in @(Get-WinEvent -FilterHashtable @{LogName=$log;StartTime=(Get-Date).AddDays(-1);Level=1,2} -MaxEvents 30 -ErrorAction Stop)) {
        $items.Add(@{log=$log;time=$item.TimeCreated.ToString('o');id=$item.Id;provider=$item.ProviderName;level=$item.LevelDisplayName})
      }
    } catch { if($_.FullyQualifiedErrorId -notlike 'NoMatchingEventsFound*'){throw} }
  }
  @($items.ToArray())
}
Section 'Servicios automáticos detenidos' {
  @(Get-CimInstance Win32_Service -Filter "StartMode='Auto' AND State<>'Running'" -ErrorAction Stop |
    Select-Object -First 60 Name,DisplayName,State,StartMode)
}
Section 'Programas de inicio (inventario)' {
  @(Get-CimInstance Win32_StartupCommand -ErrorAction Stop | Select-Object -First 100 Name,Location)
}
Section 'Espacio en discos locales' {
  @(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' -ErrorAction Stop | ForEach-Object {
    @{drive=$_.DeviceID;sizeBytes=$_.Size;freeBytes=$_.FreeSpace;freePercent=if($_.Size -gt 0){[math]::Round(100*$_.FreeSpace/$_.Size,1)}else{$null}}
  })
}
Section 'Reinicio pendiente de Windows' {
  @{pending=((Test-Path -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending') -or
    (Test-Path -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired'))}
}
@{checkedAt=[DateTimeOffset]::Now.ToString('o');sections=@($sections.ToArray());
  scope='Observaciones locales. Un servicio detenido, un error o un programa de inicio no demuestra infección. Sin detecciones no garantiza ausencia de malware. Las secciones no disponibles no se consideran correctas.'} | ConvertTo-Json -Depth 8 -Compress
