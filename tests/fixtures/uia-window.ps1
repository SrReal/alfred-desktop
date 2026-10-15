param([string]$MetadataPath,[string]$ExpectedUnicode="",[switch]$AllowActivation,[string]$InputHelperPath="")
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
Add-Type -ReferencedAssemblies System.Windows.Forms,System.Drawing -TypeDefinition @'
using System;
using System.IO;
using System.Windows.Forms;
using System.Drawing;
using System.Runtime.InteropServices;
public sealed class AlfredPetFixtureInput : TextBox {
  [DllImport("user32.dll")] static extern IntPtr GetMessageExtraInfo();
  public string ExpectedUnicode="";
  int expectedOffset=0;
  readonly System.Collections.Generic.Queue<int> unicode = new System.Collections.Generic.Queue<int>();
  protected override void WndProc(ref Message message) {
    if(message.Msg>=0x100 && message.Msg<=0x109) {
      if(message.Msg==0x102) {
        // TranslateMessage generates WM_CHAR without retaining dwExtraInfo.
        // Accept only the next explicitly armed fixture UTF-16 unit after a tagged VK_PACKET.
        if(unicode.Count==0 || unicode.Peek()!=message.WParam.ToInt32())return;
        unicode.Dequeue();
      } else {
        if(GetMessageExtraInfo().ToInt64()!=0x411f000000001234L)return;
        if(message.Msg==0x100 && message.WParam.ToInt32()==0xE7) {
          if(unicode.Count>=128 || expectedOffset>=ExpectedUnicode.Length)return;
          unicode.Enqueue(ExpectedUnicode[expectedOffset++]);
        }
      }
    }
    base.WndProc(ref message);
  }
}
public sealed class AlfredPetFixtureForm : Form {
  public bool AllowActivation=false;
  protected override bool ShowWithoutActivation { get { return !AllowActivation; } }
  protected override CreateParams CreateParams {get {var value=base.CreateParams;if(!AllowActivation)value.ExStyle|=0x08000000;return value;}}
}
public static class AlfredPetFixture {
  [StructLayout(LayoutKind.Sequential)] struct JobBasic { public long ProcessTime,JobTime; public uint Flags; public UIntPtr Minimum,Maximum; public uint Active; public UIntPtr Affinity; public uint Priority,Scheduling; }
  [StructLayout(LayoutKind.Sequential)] struct IoCounters { public ulong ReadOperations,WriteOperations,OtherOperations,ReadBytes,WriteBytes,OtherBytes; }
  [StructLayout(LayoutKind.Sequential)] struct JobLimits { public JobBasic Basic; public IoCounters Io; public UIntPtr ProcessMemory,JobMemory,PeakProcessMemory,PeakJobMemory; }
  [DllImport("kernel32.dll",CharSet=CharSet.Unicode)] static extern IntPtr CreateJobObject(IntPtr attributes,string name);
  [DllImport("kernel32.dll")] static extern bool SetInformationJobObject(IntPtr job,int information,ref JobLimits limits,uint size);
  [DllImport("kernel32.dll")] static extern bool AssignProcessToJobObject(IntPtr job,IntPtr process);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
  [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
  static IntPtr OwnedJob() {
    var job=CreateJobObject(IntPtr.Zero,null);if(job==IntPtr.Zero)throw new InvalidOperationException("FIXTURE_JOB_CREATE");
    var limits=new JobLimits();limits.Basic.Flags=0x2000;
    if(!SetInformationJobObject(job,9,ref limits,(uint)Marshal.SizeOf(limits))){CloseHandle(job);throw new InvalidOperationException("FIXTURE_JOB_LIMITS");}
    return job;
  }

  [STAThread] public static void Run(string metadata,string expectedUnicode,bool allowActivation,string inputHelper) {
    if(expectedUnicode.Length>128)throw new ArgumentException("FIXTURE_UNICODE_LIMIT");
    Application.EnableVisualStyles();
    var form = new AlfredPetFixtureForm { AllowActivation=allowActivation, Text="AlfredPet UIA Test Fixture", Size=new Size(720,540), StartPosition=FormStartPosition.CenterScreen, TopMost=false };
    var text = new AlfredPetFixtureInput { ExpectedUnicode=expectedUnicode, Name="AlfredPetFixtureInput", AccessibleName="AlfredPetFixtureInput", Location=new Point(20,30), Width=300 };
    var password = new AlfredPetFixtureInput { Name="AlfredPetFixturePassword", AccessibleName="AlfredPetFixturePassword", UseSystemPasswordChar=true, Location=new Point(20,80) };
    var button = new Button { Name="AlfredPetFixtureButton", AccessibleName="AlfredPetFixtureButton", Text="Invoke fixture", Location=new Point(20,130), Width=150 };
    var label = new Label { Text="AlfredPet OCR fixture", AccessibleName="AlfredPet OCR fixture", Location=new Point(200,135), Width=250 };
    button.Click += (s,e) => { label.Text="AlfredPet clicked"; label.AccessibleName="AlfredPet clicked"; };
    form.Controls.AddRange(new Control[] {text,password,button,label});
    var left = new Panel { Name="FixtureLeftGroup", AccessibleName="FixtureLeftGroup", Location=new Point(20,190), Size=new Size(200,80) };
    var right = new Panel { Name="FixtureRightGroup", AccessibleName="FixtureRightGroup", Location=new Point(250,190), Size=new Size(200,80) };
    left.Controls.Add(new AlfredPetFixtureInput { Name="SharedInput", AccessibleName="SharedInput", Text="left", Location=new Point(10,20), Width=170 });
    right.Controls.Add(new AlfredPetFixtureInput { Name="SharedInput", AccessibleName="SharedInput", Text="right", Location=new Point(10,20), Width=170 });
    var list = new ListBox { Name="FixtureChoices", AccessibleName="FixtureChoices", Location=new Point(480,190), Size=new Size(180,80) };
    list.Items.AddRange(new object[]{"Fixture option A","Fixture option B"});
    var selected = new Label { AccessibleName="No item selected", Text="No item selected", Location=new Point(480,280), Width=180 };
    list.SelectedIndexChanged += (s,e) => { selected.Text=Convert.ToString(list.SelectedItem);selected.AccessibleName=selected.Text; };
    var rich = new RichTextBox { Name="FixtureDocument", AccessibleName="FixtureDocument", ReadOnly=true, Text="AlfredPet TextPattern document", Location=new Point(400,20), Size=new Size(250,100) };
    var disabled = new TextBox { Name="FixtureDisabled", AccessibleName="FixtureDisabled", Enabled=false, Text="disabled", Location=new Point(20,290), Width=180 };
    var delayed = new Button { Name="FixtureDelayed", AccessibleName="FixtureDelayed", Text="Delayed control", Location=new Point(250,290), Enabled=false };
    var timer = new Timer { Interval=5000 };timer.Tick += (s,e) => {delayed.Enabled=true;timer.Stop();};
    var arm = new Button { Name="FixtureArmDelay", AccessibleName="FixtureArmDelay", Text="Arm delay", Location=new Point(480,130) };
    arm.Click += (s,e) => {delayed.Enabled=false;timer.Stop();timer.Start();};
    var source = new Panel { Name="FixtureDragSource", AccessibleName="FixtureDragSource", BackColor=Color.LightGray, Location=new Point(20,350), Size=new Size(140,80) };
    var destination = new Panel { Name="FixtureDropTarget", AccessibleName="FixtureDropTarget", BackColor=Color.Gray, AllowDrop=true, Location=new Point(220,350), Size=new Size(140,80) };
    var pointer = new Label { AccessibleName="Pointer events 0", Text="Pointer events 0", Location=new Point(400,350), Width=220 };
    int pointerEvents=0;source.MouseEnter += (s,e) => {pointerEvents++;pointer.Text="Pointer events "+pointerEvents;pointer.AccessibleName=pointer.Text;};
    source.MouseDown += (s,e) => {if(e.Button==MouseButtons.Left)source.DoDragDrop("AlfredPet synthetic fixture",DragDropEffects.Copy);};
    destination.DragEnter += (s,e) => {if(e.Data.GetDataPresent(DataFormats.Text))e.Effect=DragDropEffects.Copy;};
    destination.DragDrop += (s,e) => {if(Convert.ToString(e.Data.GetData(DataFormats.Text))=="AlfredPet synthetic fixture"){pointer.Text="AlfredPet drop received";pointer.AccessibleName=pointer.Text;}};
    form.Controls.AddRange(new Control[]{left,right,list,selected,rich,disabled,delayed,arm,source,destination,pointer});
    // The positive SendInput contract owns both windows. Its helper is a child
    // of their foreground process, rather than depending on a personal app's
    // foreground activation rights. A job terminates that child if the fixture dies.
    if(!String.IsNullOrEmpty(inputHelper)) {
      inputHelper=Path.GetFullPath(inputHelper);
      if(!allowActivation || !File.Exists(inputHelper))throw new InvalidOperationException("FIXTURE_INPUT_HELPER");
      var folder=Path.GetDirectoryName(Path.GetFullPath(metadata));
      var resultPath=Path.Combine(folder,"input-result.json");
      var observationPath=Path.Combine(folder,"input-observation.json");
      var errorPath=Path.Combine(folder,"input-error.txt");
      var runInput=new Button { Name="FixtureNativeInputContract",AccessibleName="FixtureNativeInputContract",Text="Owned input contract",Location=new Point(400,450),Width=220 };
      form.Controls.Add(runInput);
      var job=OwnedJob();Form origin=null;TextBox canary=null;System.Diagnostics.Process worker=null;
      System.Threading.Tasks.Task<string> output=null,error=null;
      var completion=new Timer {Interval=25};var attempted=false;long foregroundBefore=0,helperPid=0;
      completion.Tick += (s,e) => {
        if(worker==null || !worker.HasExited)return;
        completion.Stop();
        File.WriteAllText(resultPath,output.GetAwaiter().GetResult());
        File.WriteAllText(errorPath,error.GetAwaiter().GetResult());
        File.WriteAllText(observationPath,"{\"origin\":"+origin.Handle.ToInt64()+",\"target\":"+form.Handle.ToInt64()+",\"process_id\":"+System.Diagnostics.Process.GetCurrentProcess().Id+",\"helper_pid\":"+helperPid+",\"foreground_before\":"+foregroundBefore+",\"foreground_after\":"+GetForegroundWindow().ToInt64()+",\"origin_text_unchanged\":"+(canary.Text=="AlfredPet owned focus origin"?"true":"false")+",\"exit_code\":"+worker.ExitCode+"}");
        worker.Dispose();worker=null;
      };
      runInput.Click += (s,e) => {
        if(attempted)return;attempted=true;
        try {
          origin=new AlfredPetFixtureForm {AllowActivation=true,Text="AlfredPet owned focus origin",Size=new Size(360,160),StartPosition=FormStartPosition.CenterScreen};
          canary=new AlfredPetFixtureInput {ExpectedUnicode=" fixture",Text="AlfredPet owned focus origin",Location=new Point(20,20),Width=300};origin.Controls.Add(canary);origin.Show();origin.Activate();
          foregroundBefore=GetForegroundWindow().ToInt64();
          if(foregroundBefore!=origin.Handle.ToInt64())throw new InvalidOperationException("FIXTURE_ORIGIN_NOT_FOREGROUND");
          worker=new System.Diagnostics.Process();worker.StartInfo.FileName=Path.Combine(Environment.GetEnvironmentVariable("SystemRoot"),"System32/WindowsPowerShell/v1.0/powershell.exe");
          worker.StartInfo.Arguments="-NoProfile -NonInteractive -STA -ExecutionPolicy Bypass -File \""+inputHelper+"\"";
          worker.StartInfo.UseShellExecute=false;worker.StartInfo.CreateNoWindow=true;worker.StartInfo.RedirectStandardInput=true;worker.StartInfo.RedirectStandardOutput=true;worker.StartInfo.RedirectStandardError=true;
          if(!worker.Start())throw new InvalidOperationException("FIXTURE_INPUT_START");helperPid=worker.Id;
          if(!AssignProcessToJobObject(job,worker.Handle)){worker.Kill();throw new InvalidOperationException("FIXTURE_INPUT_JOB");}
          output=worker.StandardOutput.ReadToEndAsync();error=worker.StandardError.ReadToEndAsync();
          worker.StandardInput.Write("{\"operation\":\"gui.send_keys\",\"input\":{\"hwnd\":"+form.Handle.ToInt64()+",\"process_id\":"+System.Diagnostics.Process.GetCurrentProcess().Id+",\"name\":\"AlfredPetFixtureInput\",\"_input_tag\":\"4692469336743350836\",\"text\":\" fixture\",\"body\":{\"focus_and_restore\":true}}}");worker.StandardInput.Close();completion.Start();
        } catch(Exception failure) {File.WriteAllText(errorPath,failure.ToString());File.WriteAllText(resultPath,"{\"ok\":false,\"error\":\"FIXTURE_INPUT_SETUP_FAILED\"}");}
      };
      form.FormClosed += (s,e) => {completion.Stop();completion.Dispose();if(job!=IntPtr.Zero){CloseHandle(job);job=IntPtr.Zero;}if(worker!=null)worker.Dispose();if(origin!=null)origin.Close();};
    }
    form.Shown += (s,e) => File.WriteAllText(metadata, "{\"hwnd\":"+form.Handle.ToInt64()+",\"process_id\":"+System.Diagnostics.Process.GetCurrentProcess().Id+",\"controls\":"+form.Controls.Count+"}");
    Application.Run(form);
  }
}
'@
[AlfredPetFixture]::Run($MetadataPath,$ExpectedUnicode,$AllowActivation.IsPresent,$InputHelperPath)
