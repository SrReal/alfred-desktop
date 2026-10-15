using System;
using System.Collections.Generic;
using System.Linq;
using System.Speech.Recognition;
using System.Threading;
using System.Threading.Tasks;
using System.Web.Script.Serialization;

namespace AlfredPet.Dictation {
  // In-process Windows recognition. Audio is neither saved nor sent to a service.
  public static class Worker {
    [STAThread]
    public static int Main(string[] args) {
      Console.OutputEncoding=new System.Text.UTF8Encoding(false);
      Console.InputEncoding=new System.Text.UTF8Encoding(false);
      if(args.Length!=1 || (args[0]!="info" && args[0]!="listen")) return 2;
      try { if(args[0]=="info") Info(); else Run(); return 0; }
      catch { Emit(new { type="error", code="DICTATION_ENGINE_UNAVAILABLE" }); return 1; }
    }
    static readonly object gate = new object();
    static void Emit(object value) {
      lock(gate) { Console.WriteLine(new JavaScriptSerializer().Serialize(value)); Console.Out.Flush(); }
    }
    static RecognizerInfo Spanish() {
      return SpeechRecognitionEngine.InstalledRecognizers()
        .OrderByDescending(r => r.Culture.Name == "es-ES")
        .FirstOrDefault(r => r.Culture.TwoLetterISOLanguageName == "es");
    }
    public static void Info() {
      var r = Spanish();
      Emit(new { available=r!=null, culture=r==null ? null : r.Culture.Name, engine="Windows System.Speech", maxSeconds=120 });
    }
    public static void Run() {
      var info=Spanish();
      if(info==null) { Emit(new { type="error", code="DICTATION_SPANISH_REQUIRED" }); return; }
      using(var done=new ManualResetEvent(false))
      using(var recognizer=new SpeechRecognitionEngine(info)) {
        int characters=0; bool limit=false;
        recognizer.LoadGrammar(new DictationGrammar());
        recognizer.SpeechRecognized += (sender,e) => {
          if(String.IsNullOrWhiteSpace(e.Result.Text)) return;
          lock(gate) {
            if(limit) return;
            characters+=e.Result.Text.Length+1;
            if(characters>12000) { limit=true; Emit(new { type="error", code="DICTATION_TEXT_LIMIT" }); return; }
            Emit(new { type="text", text=e.Result.Text });
          }
        };
        recognizer.RecognizeCompleted += (sender,e) => {
          if(e.Error!=null) Emit(new { type="error", code="DICTATION_RECOGNITION_FAILED" });
          done.Set();
        };
        try { recognizer.SetInputToDefaultAudioDevice(); }
        catch { Emit(new { type="error", code="DICTATION_MICROPHONE_UNAVAILABLE" }); return; }
        recognizer.RecognizeAsync(RecognizeMode.Multiple);
        Emit(new { type="ready", culture=info.Culture.Name });
        // EOF means the application disappeared. No restart or background listening.
        var command=Task.Factory.StartNew(() => Console.ReadLine());
        var deadline=DateTime.UtcNow.AddSeconds(120);
        bool stopped=false;
        while(!done.WaitOne(50)) {
          if(!stopped && (command.IsCompleted || DateTime.UtcNow>=deadline || limit)) {
            stopped=true;
            if(command.IsCompleted && command.Result!="stop") recognizer.RecognizeAsyncCancel();
            else recognizer.RecognizeAsyncStop();
            deadline=DateTime.UtcNow.AddSeconds(5);
          }
          if(stopped && DateTime.UtcNow>=deadline) {
            recognizer.RecognizeAsyncCancel();
            Emit(new { type="error", code="DICTATION_STOP_TIMEOUT" });
            break;
          }
        }
        recognizer.SetInputToNull();
        Emit(new { type="complete" });
      }
    }
  }
}
