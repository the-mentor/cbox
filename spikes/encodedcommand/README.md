# Spike: Windows Credential Manager through `powershell.exe -EncodedCommand`

Throwaway. Answers the open question in Task 5, Step 1 of the secrets plan: does
the embedded-script design work on **Windows PowerShell 5.1 under real WSL**?

Run inside WSL (any distro with interop on), from this directory:

    bash run.sh

Needs only `bash`, `iconv`, `base64`, `powershell.exe`. It writes test entries
named `cbox-spike/<random>` to Windows Credential Manager and deletes them on
exit. The test secrets are dummy strings; nothing real is read or printed.

Send back the full output (it contains no secret values). If check 1 fails
(stdin not reaching the script), stop: the design needs a rethink.
