Option Explicit
Dim files, shell, folder, executable
Set files = CreateObject("Scripting.FileSystemObject")
Set shell = CreateObject("WScript.Shell")
folder = files.GetParentFolderName(WScript.ScriptFullName)
executable = files.BuildPath(folder, "game.exe")
If Not files.FileExists(executable) Then
    executable = files.BuildPath(folder, "dist\windows\game.exe")
End If
If Not files.FileExists(executable) Then
    MsgBox "No Windows game package was found. Run the verifier with -Package first.", 48, "Relay Yard"
    WScript.Quit 1
End If
shell.CurrentDirectory = files.GetParentFolderName(executable)
shell.Run Chr(34) & executable & Chr(34), 1, False
