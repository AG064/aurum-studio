Set files = CreateObject("Scripting.FileSystemObject")
Set shell = CreateObject("WScript.Shell")
root = files.GetParentFolderName(WScript.ScriptFullName)
Set environment = shell.Environment("Process")
environment("AURUM_STUDIO_HOME") = root
shell.CurrentDirectory = root
shell.Run Chr(34) & files.BuildPath(root, "bin\aurum.exe") & Chr(34) & " studio", 0, False
