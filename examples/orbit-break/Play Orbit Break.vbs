Option Explicit
Dim fs, shell, root, executable
Set fs = CreateObject("Scripting.FileSystemObject")
Set shell = CreateObject("WScript.Shell")
root = fs.GetParentFolderName(WScript.ScriptFullName)
executable = fs.BuildPath(root, "dist\windows\game.exe")
If Not fs.FileExists(executable) Then
    MsgBox "The Windows package is missing. See README.md for the verify.ps1 -Package -OutputDirectory command, or package the project through Aurum Studio.", 48, "Orbit Break"
    WScript.Quit 1
End If
shell.CurrentDirectory = fs.GetParentFolderName(executable)
shell.Run Chr(34) & executable & Chr(34), 1, False
