using System;
using System.IO;
using System.Collections.Generic;
using System.Drawing;
using System.Windows.Forms;
using System.Runtime.InteropServices;
using System.Web.Script.Serialization;
public sealed class RecoveryFixtureWindow : Form {
 public bool Veto,BlockPosition;public string BlockedPath,ReleasePath;public int PositionMessages;
 [StructLayout(LayoutKind.Sequential)] struct WINDOWPOS { public IntPtr hwnd,after;public int x,y,cx,cy;public uint flags; }
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern IntPtr RemoveProp(IntPtr hwnd,string name);
 protected override bool ShowWithoutActivation {get{return true;}}
 protected override CreateParams CreateParams {get{var p=base.CreateParams;p.ExStyle|=0x08000000;return p;}}
 protected override void WndProc(ref Message m) {
  if(m.Msg==0x46)PositionMessages++;
  if(m.Msg==0x46 && BlockPosition){BlockPosition=false;File.WriteAllText(BlockedPath,"blocked");var limit=DateTime.UtcNow.AddSeconds(30);while(!File.Exists(ReleasePath) && DateTime.UtcNow<limit)System.Threading.Thread.Sleep(10); }
  if(m.Msg==0x46 && Veto){var p=(WINDOWPOS)Marshal.PtrToStructure(m.LParam,typeof(WINDOWPOS));p.flags|=3;Marshal.StructureToPtr(p,m.LParam,false);}
  if(m.Msg==0x82)RemoveProp(Handle,"AlfredPet.WindowIdentity.v1");
  base.WndProc(ref m);
 }
}
public static class RecoveryFixture {
 [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
 [StructLayout(LayoutKind.Sequential)] struct Point {public int x,y;}
 [StructLayout(LayoutKind.Sequential)] struct Rect {public int left,top,right,bottom;}
 [StructLayout(LayoutKind.Sequential)] struct Placement {public uint length,flags,show;public Point min,max;public Rect normal;}
 [DllImport("user32.dll")] static extern bool GetWindowPlacement(IntPtr hwnd,ref Placement placement);
 [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd,out Rect rect);
 [DllImport("user32.dll",EntryPoint="GetWindowLongW")] static extern int GetWindowLong(IntPtr hwnd,int index);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern IntPtr GetProp(IntPtr hwnd,string name);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern bool SetProp(IntPtr hwnd,string name,IntPtr value);
 [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd,IntPtr after,int x,int y,int width,int height,uint flags);
 static object Snapshot(RecoveryFixtureWindow f,Placement p,System.Diagnostics.Process owner){
  Rect rect;if(!GetWindowRect(f.Handle,out rect))throw new InvalidOperationException("Owned bounds probe failed");
  var screen=Screen.FromHandle(f.Handle);int dx=0,dy=0;if((GetWindowLong(f.Handle,-20)&0x80)==0){dx=screen.WorkingArea.X-screen.Bounds.X;dy=screen.WorkingArea.Y-screen.Bounds.Y;}
  var placement=new Dictionary<string,object>{{"show_cmd",p.show},{"min_x",p.show==2?p.min.x:-1},{"min_y",p.show==2?p.min.y:-1},{"max_x",p.show==3?p.max.x:-1},{"max_y",p.show==3?p.max.y:-1},{"left",p.normal.left},{"top",p.normal.top},{"right",p.normal.right},{"bottom",p.normal.bottom}};
  if(p.show==2&&(p.flags&2)!=0)placement.Add("restore_maximized",true);
  var identity=GetProp(f.Handle,"AlfredPet.WindowIdentity.v1");
  return new {hwnd=f.Handle.ToInt64(),process_id=owner.Id,process_started=owner.StartTime.ToUniversalTime().Ticks.ToString(System.Globalization.CultureInfo.InvariantCulture),window_identity=identity==IntPtr.Zero?null:identity.ToInt64().ToString("x16",System.Globalization.CultureInfo.InvariantCulture),monitor=screen.DeviceName,x=rect.left,y=rect.top,width=rect.right-rect.left,height=rect.bottom-rect.top,normal_screen=new{x=p.normal.left+dx,y=p.normal.top+dy,width=p.normal.right-p.normal.left,height=p.normal.bottom-p.normal.top},placement=placement};
 }
 static void ExternalMove(){
  var raw=File.ReadAllText(commands+".move");if(raw.Length>16384)throw new InvalidOperationException("Owned move input limit");
  var value=new JavaScriptSerializer().Deserialize<Dictionary<string,object>>(raw);var hwnd=Convert.ToInt64(value["hwnd"]);var form=forms.Find(f=>f.Handle.ToInt64()==hwnd);if(form==null)throw new InvalidOperationException("Owned move target missing");
  var fields=new int[4];var keys=new[]{"x","y","width","height"};for(int i=0;i<4;i++){fields[i]=Convert.ToInt32(value[keys[i]]);if(Math.Abs((long)fields[i])>100000)throw new InvalidOperationException("Owned move bounds");}if(fields[2]<1||fields[3]<1)throw new InvalidOperationException("Owned move size");
  if(!SetWindowPos(form.Handle,IntPtr.Zero,fields[0],fields[1],fields[2],fields[3],20))throw new InvalidOperationException("Owned external move failed");
 }
 static readonly List<RecoveryFixtureWindow> forms=new List<RecoveryFixtureWindow>();
 static string metadata,commands;static int revision;
 static RecoveryFixtureWindow New(int slot,Rectangle bounds){
  var f=new RecoveryFixtureWindow {Text="AlfredPet recovery fixture "+slot,BlockedPath=metadata+".blocked",ReleasePath=metadata+".release",StartPosition=FormStartPosition.Manual,Bounds=bounds};
  f.Show();return f;
 }
 static void Report(){
  var rows=new List<object>();using(var owner=System.Diagnostics.Process.GetCurrentProcess()){foreach(var f in forms){var p=new Placement {length=(uint)Marshal.SizeOf(typeof(Placement))};if(!GetWindowPlacement(f.Handle,ref p))throw new InvalidOperationException("Owned placement probe failed");rows.Add(new {hwnd=f.Handle.ToInt64(),process_id=owner.Id,process_started=owner.StartTime.ToUniversalTime().Ticks.ToString(System.Globalization.CultureInfo.InvariantCulture),placement_flags=p.flags,show_cmd=p.show,position_messages=f.PositionMessages,snapshot=Snapshot(f,p,owner)});}}
  File.WriteAllText(metadata,new JavaScriptSerializer().Serialize(new {revision=revision,windows=rows}),new System.Text.UTF8Encoding(false));
 }
 static string ConsumeCommand(){
  try {
   using(var stream=new FileStream(commands,FileMode.OpenOrCreate,FileAccess.ReadWrite,FileShare.None)){
    if(stream.Length>32)throw new InvalidOperationException("Owned command exceeds bounds");
    string command;using(var reader=new StreamReader(stream,new System.Text.UTF8Encoding(false,true),true,1024,true)){command=reader.ReadToEnd().Trim();}
    if(command.Length>0)stream.SetLength(0);
    return command;
   }
  }catch(IOException error){var code=error.HResult&0xffff;if(code==32||code==33)return "";throw;}
 }
 [STAThread] public static void Main(string[] args){if(args.Length!=2)throw new ArgumentException("Owned paths required");Run(args[0],args[1]);}
 [STAThread] public static void Run(string m,string c){
  SetThreadDpiAwarenessContext(new IntPtr(-4));
  metadata=m;commands=c;Application.EnableVisualStyles();
  var area=Screen.PrimaryScreen.WorkingArea;
  forms.Add(New(0,new Rectangle(area.X+80,area.Y+80,460,340)));
  forms.Add(New(1,new Rectangle(area.X+130,area.Y+130,460,340)));
  Report();var timer=new Timer {Interval=50};
  timer.Tick+=(sender,args)=>{
   var command=ConsumeCommand();if(command.Length==0)return;
   if(command=="report"){}
   else if(command=="initialize"){foreach(var f in forms){if(GetProp(f.Handle,"AlfredPet.WindowIdentity.v1")==IntPtr.Zero){var value=BitConverter.ToInt64(Guid.NewGuid().ToByteArray(),0)&long.MaxValue;if(value==0||!SetProp(f.Handle,"AlfredPet.WindowIdentity.v1",new IntPtr(value)))throw new InvalidOperationException("Owned identity init failed");}}}
   else if(command=="move")ExternalMove();
   else if(command=="later_writer")forms[0].Left+=7;
   else if(command=="block_move")forms[0].BlockPosition=true;
   else if(command=="veto")forms[0].Veto=true;
   else if(command=="allow")forms[0].Veto=false;
   else if(command=="recreate"){
    var old=forms[0];var bounds=old.Bounds;old.Close();old.Dispose();forms[0]=New(0,bounds);
   }else throw new InvalidOperationException("Unknown owned fixture command");
   revision++;Report();
  };
  timer.Start();try{Application.Run();}finally{timer.Dispose();foreach(var f in forms){f.Close();f.Dispose();}}
 }
}
