param([string]$UiaSource)
$ErrorActionPreference='Stop'
# Own window only. Every read is bound to its HWND and fails if the user changes focus.
$source = [IO.File]::ReadAllText($UiaSource)
$fixture = @'
namespace AlfredPet.Tests {
 public static class ExplainSelection {
  [System.Runtime.InteropServices.DllImport("user32.dll")] static extern bool SetForegroundWindow(System.IntPtr window);
  public static string Run() {
   string result=null;System.Exception failure=null;
   var ui=new System.Threading.Thread(()=>{
    try{
     var form=new System.Windows.Window{Title="Owned selection fixture",Width=500,Height=250};
     var rich=new System.Windows.Controls.TextBox{Text="Before. Selected fragment. After.",Height=120};
     System.Windows.Automation.AutomationProperties.SetAutomationId(rich,"OwnedSelection");
     var password=new System.Windows.Controls.PasswordBox{Password="owned-test-only",Height=30};
     System.Windows.Automation.AutomationProperties.SetAutomationId(password,"OwnedPassword");
     var panel=new System.Windows.Controls.StackPanel();panel.Children.Add(rich);panel.Children.Add(password);form.Content=panel;
     var timer=new System.Windows.Threading.DispatcherTimer{Interval=System.TimeSpan.FromMilliseconds(500)};
     form.ContentRendered+=(s,e)=>timer.Start();
     timer.Tick+=(s,e)=>{
      timer.Stop(); form.Activate(); SetForegroundWindow(new System.Windows.Interop.WindowInteropHelper(form).Handle);rich.Focus();rich.Select(8,18);
      long hwnd=new System.Windows.Interop.WindowInteropHelper(form).Handle.ToInt64();
      var read=new System.Threading.Thread(()=>{
       try{
        string text; using(var target=AlfredPet.UIA.Target.Find(hwnd,"","OwnedSelection")){text=target.GetSelection();}
        if(text!="Selected fragment.")throw new System.Exception("WRONG_SELECTION: "+text);
        bool changed=false;try{AlfredPet.UIA.Target.FocusedSelection(hwnd+1);}catch{changed=true;}
        if(!changed)throw new System.Exception("TARGET_CHANGE_NOT_REJECTED");
        form.Dispatcher.Invoke(new System.Action(()=>{password.Focus();password.SelectAll();}));
        bool denied=false;try{using(var target=AlfredPet.UIA.Target.Find(hwnd,"","OwnedPassword")){target.GetSelection();}}catch(System.Exception err){denied=err.ToString().Contains("PASSWORD_DENIED");}
        if(!denied)throw new System.Exception("PASSWORD_NOT_REJECTED");
        result="{\"status\":\"PASS\",\"checks\":3,\"scope\":\"Owned HWND only: selected range, wrong foreground target, password rejection; physical shortcut/focus not exercised; no clipboard access\"}";
       }catch(System.Exception err){failure=err;}
       finally{form.Dispatcher.BeginInvoke(new System.Action(()=>form.Close()));}
      });read.SetApartmentState(System.Threading.ApartmentState.MTA);read.Start();
     };
     form.ShowDialog();
    }catch(System.Exception err){failure=err;}
   });
   ui.SetApartmentState(System.Threading.ApartmentState.STA);ui.Start();
   if(!ui.Join(15000))throw new System.Exception("OWNED_SELECTION_TIMEOUT");
   if(failure!=null)throw failure;
   return result;
  }
 }
}
'@
$wpf=Join-Path ([Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory()) 'WPF'
Add-Type -ReferencedAssemblies (Join-Path $wpf 'PresentationFramework.dll'),(Join-Path $wpf 'PresentationCore.dll'),(Join-Path $wpf 'WindowsBase.dll'),System.Xaml -TypeDefinition ($source+"`n"+$fixture)
[AlfredPet.Tests.ExplainSelection]::Run()
