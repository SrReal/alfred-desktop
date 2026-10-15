using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

// Interface order and IDs follow the Windows SDK UIAutomationClient.h.
// Unused prefix slots deliberately retain their position in the COM vtable.
namespace AlfredPet.UIA {
  [ComImport, Guid("352ffba8-0973-437c-a61f-f64cafd81df9"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface Condition { }
  [ComImport, Guid("30cbe57d-d9d0-452a-ab13-7ac5ac4825ee"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface Automation {
    [return: MarshalAs(UnmanagedType.Bool)] bool CompareElements(Element first,Element second); void CompareRuntimeIds();
    Element GetRootElement(); Element ElementFromHandle(IntPtr hwnd);
    Element ElementFromPoint(Point point); Element GetFocusedElement();
    void GetRootElementBuildCache(); void ElementFromHandleBuildCache();
    void ElementFromPointBuildCache(); void GetFocusedElementBuildCache();
    void CreateTreeWalker(); Walker GetControlViewWalker(); void GetContentViewWalker();
    void GetRawViewWalker(); void GetRawViewCondition(); void GetControlViewCondition();
    void GetContentViewCondition(); void CreateCacheRequest();
    Condition CreateTrueCondition(); void CreateFalseCondition();
    Condition CreatePropertyCondition(int property, [MarshalAs(UnmanagedType.Struct)] object value);
  }
  [StructLayout(LayoutKind.Sequential)] public struct Point { public int X,Y; }
  [ComImport, Guid("4042c624-389c-4afc-a630-9df854a541fc"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface Walker {
    Element GetParentElement(Element element); Element GetFirstChildElement(Element element);
    Element GetLastChildElement(Element element); Element GetNextSiblingElement(Element element);
  }
  public sealed class Selector {
    public string Name,AutomationId,Scope; public int ControlType;
    public Selector(string name,string id,int type,string scope){
      if((name??"").Length>1024 || (id??"").Length>1024 || (type!=0 && (type<50000 || type>50040)) || (scope!="children" && scope!="descendants" && scope!="self"))throw new InvalidOperationException("UIA_SELECTOR_INVALID");
      Name=name;AutomationId=id;ControlType=type;Scope=scope;
    }
    public bool Empty { get{return String.IsNullOrEmpty(Name) && String.IsNullOrEmpty(AutomationId) && ControlType==0;} }
    public bool Matches(Element e){return (String.IsNullOrEmpty(Name) || String.Equals(Name,Convert.ToString(e.GetCurrentPropertyValue(30005)),StringComparison.Ordinal)) && (String.IsNullOrEmpty(AutomationId) || String.Equals(AutomationId,Convert.ToString(e.GetCurrentPropertyValue(30011)),StringComparison.Ordinal)) && (ControlType==0 || ControlType==Convert.ToInt32(e.GetCurrentPropertyValue(30003)));}
  }
  [ComImport, Guid("d22108aa-8ac5-49a5-837b-37bbb3d7591e"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface Element {
    void SetFocus(); void GetRuntimeId();
    Element FindFirst(int scope, Condition condition); Elements FindAll(int scope, Condition condition);
    void FindFirstBuildCache(); void FindAllBuildCache(); void BuildUpdatedCache();
    [return: MarshalAs(UnmanagedType.Struct)] object GetCurrentPropertyValue(int property);
    void GetCurrentPropertyValueEx(); void GetCachedPropertyValue(); void GetCachedPropertyValueEx();
    void GetCurrentPatternAs(); void GetCachedPatternAs();
    [return: MarshalAs(UnmanagedType.IUnknown)] object GetCurrentPattern(int pattern);
  }
  [ComImport, Guid("14314595-b4bc-4055-95f2-58f2e42c9855"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface Elements { int Length { get; } Element GetElement(int index); }
  [ComImport, Guid("fb377fbe-8ea6-46d5-9c73-6499642d3059"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface InvokePattern { void Invoke(); }
  [ComImport, Guid("a94cd8b1-0844-4cd6-9d2d-640537ab39e9"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface ValuePattern {
    void SetValue([MarshalAs(UnmanagedType.BStr)] string value);
    [return: MarshalAs(UnmanagedType.BStr)] string GetCurrentValue();
  }
  [ComImport, Guid("a8efa66a-0fda-421a-9194-38021f3578ea"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface SelectionPattern { void Select(); }
  [ComImport, Guid("32eba289-3583-42c9-9c59-3b6d9a1e9b6a"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface TextPattern {
    void RangeFromPoint(); void RangeFromChild(); TextRanges GetSelection(); void GetVisibleRanges();
    TextRange DocumentRange { get; }
  }
  [ComImport, Guid("a543cc6a-f4ae-494b-8239-c814481187a8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface TextRange {
    void Clone(); void Compare(); void CompareEndpoints(); void ExpandToEnclosingUnit();
    void FindAttribute(); void FindText(); void GetAttributeValue();
    void GetBoundingRectangles(); void GetEnclosingElement();
    [return: MarshalAs(UnmanagedType.BStr)] string GetText(int limit);
  }
  [ComImport, Guid("ce4ae76a-e717-4c98-81ea-47371d028eb6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
  public interface TextRanges { int Length { get; } TextRange GetElement(int index); }
  public sealed class Target : IDisposable {
    private Automation automation;
    private Element element;
    private long hwnd; private int processId,selectedProcessId;
    [DllImport("user32.dll")] private static extern bool IsWindow(IntPtr handle);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr handle,out uint pid);
    private Target(Automation a, Element e,long handle,int pid) { automation=a; element=e; hwnd=handle;processId=pid;selectedProcessId=Convert.ToInt32(e.GetCurrentPropertyValue(30002)); }
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    public static string FocusedSelection(long hwnd) {
      if(hwnd==0 || GetForegroundWindow().ToInt64()!=hwnd)throw new InvalidOperationException("EXPLAIN_TARGET_CHANGED_FOREGROUND");
      Automation a=null; Element root=null,focus=null,parent=null; Walker walker=null;
      try {
        a=(Automation)Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("ff48dba4-60ef-4201-aa87-54103eef594e")));
        root=a.ElementFromHandle(new IntPtr(hwnd));focus=a.GetFocusedElement();
        if(focus==null)throw new InvalidOperationException("EXPLAIN_NO_SELECTION");
        // Validate ancestry without inspecting names, values or the full document.
        parent=Retain(focus);walker=a.GetControlViewWalker();bool belongs=false;
        for(int depth=0;parent!=null && depth<32;depth++){
          if(Convert.ToBoolean(parent.GetCurrentPropertyValue(30019)))throw new InvalidOperationException("UIA_PASSWORD_DENIED");
          if(a.CompareElements(parent,root)){belongs=true;break;}
          var next=walker.GetParentElement(parent);Release(parent);parent=next;
        }
        if(!belongs)throw new InvalidOperationException("EXPLAIN_TARGET_CHANGED_ANCESTRY");
        var text=SelectedText(focus);
        if(GetForegroundWindow().ToInt64()!=hwnd || !Convert.ToBoolean(focus.GetCurrentPropertyValue(30008)))throw new InvalidOperationException("EXPLAIN_TARGET_CHANGED_FOCUS");
        return text;
      } finally {Release(parent);Release(walker);Release(focus);Release(root);Release(a);}
    }
    public string GetSelection() { ValidateIdentity(); return SelectedText(element); }
    private static string SelectedText(Element selected) {
      object pattern=null; TextRanges ranges=null;
      try {
        pattern=selected.GetCurrentPattern(10014);
        if(pattern==null)throw new InvalidOperationException("EXPLAIN_NO_SELECTION");
        ranges=((TextPattern)pattern).GetSelection();
        if(ranges==null || ranges.Length<1 || ranges.Length>16)throw new InvalidOperationException("EXPLAIN_NO_SELECTION");
        var parts=new List<string>();int length=0;
        for(int n=0;n<ranges.Length;n++){
          TextRange range=ranges.GetElement(n);
          try {var text=range.GetText(12001);length+=text.Length;if(length>12000)throw new InvalidOperationException("EXPLAIN_SELECTION_LIMIT");parts.Add(text);}
          finally{Release(range);}
        }
        return String.Join("\n",parts.ToArray());
      } finally {Release(ranges);Release(pattern);}
    }
    public static Target Find(long hwnd, string name, string automationId) {
      return FindSelector(hwnd,0,new Selector[0],new Selector(name,automationId,0,"descendants"));
    }
    public static Target FindSelector(long hwnd,int expectedProcess,Selector[] path,Selector leaf){
      if (hwnd == 0) throw new InvalidOperationException("UIA_TARGET_HWND_REQUIRED");
      if(path==null || path.Length>8 || leaf==null)throw new InvalidOperationException("UIA_SELECTOR_INVALID");
      var a=(Automation)Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("ff48dba4-60ef-4201-aa87-54103eef594e")));
      Element root=null;
      try {
        root=a.ElementFromHandle(new IntPtr(hwnd));
        uint owner;GetWindowThreadProcessId(new IntPtr(hwnd),out owner);int pid=checked((int)owner);
        if(expectedProcess!=0 && expectedProcess!=pid)throw new InvalidOperationException("UIA_TARGET_CHANGED");
        foreach(var selector in path){
          if(selector==null || selector.Empty)throw new InvalidOperationException("UIA_SELECTOR_INVALID");
          var next=FindUnique(a,root,selector);Release(root);root=next;
          if(Convert.ToBoolean(root.GetCurrentPropertyValue(30019)))throw new InvalidOperationException("UIA_PASSWORD_DENIED");
        }
        if(!leaf.Empty){var next=FindUnique(a,root,leaf);Release(root);root=next;}
        if (Convert.ToBoolean(root.GetCurrentPropertyValue(30019)))
          throw new InvalidOperationException("UIA_PASSWORD_DENIED");
        var result=new Target(a,root,hwnd,pid);
        result.ValidateIdentity();root=null; a=null;
        return result;
      } finally {
        Release(root); Release(a);
      }
    }
    private static Element FindUnique(Automation a,Element root,Selector selector){
      Walker walker=null;Element selected=null;int visited=0;
      try{
        walker=a.GetControlViewWalker();
        if(selector.Scope=="self"){
          if(!selector.Matches(root))throw new InvalidOperationException("UIA_ELEMENT_NOT_FOUND");
          // Obtain a separate reference so replacing an ancestor does not release the result.
          selected=Retain(root);
        }else Search(walker,root,selector,0,ref visited,ref selected);
        if(selected==null)throw new InvalidOperationException("UIA_ELEMENT_NOT_FOUND");
        var result=selected;selected=null;return result;
      }finally{Release(selected);Release(walker);}
    }
    private static void Search(Walker walker,Element parent,Selector selector,int depth,ref int visited,ref Element selected){
      if(depth>=16)throw new InvalidOperationException("UIA_DEPTH_LIMIT");
      Element child=walker.GetFirstChildElement(parent);
      try{
        while(child!=null){
          if(++visited>512)throw new InvalidOperationException("UIA_NODE_LIMIT");
          if(selector.Matches(child)){
            if(selected!=null)throw new InvalidOperationException("UIA_SELECTOR_AMBIGUOUS");
            // Search owns each traversal reference; retain a separate COM reference for the match.
            selected=Retain(child);
          }
          if(selector.Scope=="descendants" && !Convert.ToBoolean(child.GetCurrentPropertyValue(30019)))Search(walker,child,selector,depth+1,ref visited,ref selected);
          var next=walker.GetNextSiblingElement(child);Release(child);child=next;
        }
      }finally{Release(child);}
    }
    public void ValidateIdentity(){
      uint actual;
      if(!IsWindow(new IntPtr(hwnd)) || GetWindowThreadProcessId(new IntPtr(hwnd),out actual)==0 || actual!=(uint)processId || Convert.ToInt32(element.GetCurrentPropertyValue(30002))!=selectedProcessId)throw new InvalidOperationException("UIA_TARGET_CHANGED");
      if(Convert.ToBoolean(element.GetCurrentPropertyValue(30019)))throw new InvalidOperationException("UIA_PASSWORD_DENIED");
    }
    public void ValidateAction(){ValidateIdentity();if(!Convert.ToBoolean(element.GetCurrentPropertyValue(30010)))throw new InvalidOperationException("UIA_ELEMENT_DISABLED");}
    public Dictionary<string,object> Metadata() { ValidateIdentity();return Describe(element); }
    private static Dictionary<string,object> Describe(Element e) {
      var result=new Dictionary<string,object> {
        {"name",e.GetCurrentPropertyValue(30005)}, {"automation_id",e.GetCurrentPropertyValue(30011)},
        {"control_type",e.GetCurrentPropertyValue(30003)}, {"enabled",e.GetCurrentPropertyValue(30010)},
        {"hwnd",e.GetCurrentPropertyValue(30020)}, {"process_id",e.GetCurrentPropertyValue(30002)},
        {"offscreen",e.GetCurrentPropertyValue(30022)}, {"rect",e.GetCurrentPropertyValue(30001)}
      };
      if(Convert.ToBoolean(e.GetCurrentPropertyValue(30036)))result["selected"]=Convert.ToBoolean(e.GetCurrentPropertyValue(30079));
      return result;
    }
    public Dictionary<string,object>[] Inspect() {
      var result=new List<Dictionary<string,object>>();
      ValidateIdentity();Walker walker=null;int visited=0;
      try {
        walker=automation.GetControlViewWalker();
        InspectNode(walker,element,-1,0,ref visited,result);
        return result.ToArray();
      } finally { Release(walker); }
    }
    private static void InspectNode(Walker walker,Element e,int parent,int depth,ref int visited,List<Dictionary<string,object>> output){
      if(++visited>512 || depth>16)throw new InvalidOperationException("UIA_TREE_LIMIT");
      if(Convert.ToBoolean(e.GetCurrentPropertyValue(30019)))return;
      var row=Describe(e);row["parent_index"]=parent;row["depth"]=depth;int index=output.Count;output.Add(row);
      Element child=walker.GetFirstChildElement(e);
      try{while(child!=null){InspectNode(walker,child,index,depth+1,ref visited,output);var next=walker.GetNextSiblingElement(child);Release(child);child=next;}}finally{Release(child);}
    }
    public double[] Bounds(){
      ValidateIdentity();if(Convert.ToBoolean(element.GetCurrentPropertyValue(30022)))throw new InvalidOperationException("UIA_ELEMENT_OFFSCREEN");
      var rect=(Array)element.GetCurrentPropertyValue(30001);if(rect.Length!=4)throw new InvalidOperationException("UIA_BOUNDS_INVALID");
      var result=new double[4];for(int n=0;n<4;n++){result[n]=Convert.ToDouble(rect.GetValue(n));if(Double.IsNaN(result[n]) || Double.IsInfinity(result[n]) || Math.Abs(result[n])>100000)throw new InvalidOperationException("UIA_BOUNDS_INVALID");}
      if(result[2]<1 || result[3]<1)throw new InvalidOperationException("UIA_BOUNDS_INVALID");return result;
    }
    public void ValidatePoint(int x,int y){
      ValidateAction();Element hit=null;Walker walker=null;
      try{
        hit=automation.ElementFromPoint(new Point{X=x,Y=y});walker=automation.GetControlViewWalker();
        for(int depth=0;hit!=null && depth<=16;depth++){
          if(Convert.ToBoolean(hit.GetCurrentPropertyValue(30019)))throw new InvalidOperationException("UIA_PASSWORD_DENIED");
          if(automation.CompareElements(hit,element))return;
          var parent=walker.GetParentElement(hit);Release(hit);hit=parent;
        }
        throw new InvalidOperationException("INPUT_TARGET_OCCLUDED");
      }finally{Release(hit);Release(walker);}
    }
    public void SetFocus() { ValidateAction();element.SetFocus(); }
    public bool HasKeyboardFocus { get { return Convert.ToBoolean(element.GetCurrentPropertyValue(30008)); } }
    public void Invoke() {
      ValidateAction();
      object pattern=element.GetCurrentPattern(10000);
      try { ((InvokePattern)pattern).Invoke(); } finally { Release(pattern); }
    }
    public bool TryInvoke(){
      ValidateAction();object pattern=null;
      try{try{pattern=element.GetCurrentPattern(10000);}catch(COMException error){if(error.ErrorCode!=unchecked((int)0x80040204))throw;}
        if(pattern==null)return false;((InvokePattern)pattern).Invoke();return true;
      }finally{Release(pattern);}
    }
    public void SetValue(string text) {
      ValidateAction();
      object pattern=element.GetCurrentPattern(10002);
      try { ((ValuePattern)pattern).SetValue(text); } finally { Release(pattern); }
    }
    public string GetText() {
      ValidateIdentity();
      object pattern=null; TextRange range=null;
      try {
        try { pattern=element.GetCurrentPattern(10014); } catch(COMException) { }
        if(pattern!=null) { range=((TextPattern)pattern).DocumentRange; return range.GetText(12000); }
        pattern=element.GetCurrentPattern(10002);
        string value=((ValuePattern)pattern).GetCurrentValue();
        return value.Substring(0,Math.Min(12000,value.Length));
      } finally { Release(range); Release(pattern); }
    }
    public void Select() {
      ValidateAction();
      object pattern=element.GetCurrentPattern(10010);
      try { ((SelectionPattern)pattern).Select();
        for(int attempt=0;attempt<25;attempt++){ValidateIdentity();if(Convert.ToBoolean(element.GetCurrentPropertyValue(30079)))return;System.Threading.Thread.Sleep(20);}
        throw new InvalidOperationException("UIA_SELECTION_READBACK_FAILED");
      } finally { Release(pattern); }
    }
    private static Element Retain(Element value){var ptr=Marshal.GetIUnknownForObject(value);try{return (Element)Marshal.GetUniqueObjectForIUnknown(ptr);}finally{Marshal.Release(ptr);}}
    private static void Release(object value) { if(value!=null && Marshal.IsComObject(value)) Marshal.ReleaseComObject(value); }
    public void Dispose() { Release(element); Release(automation); element=null; automation=null; }
  }
}
