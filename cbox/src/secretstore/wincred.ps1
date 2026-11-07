$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class WinCred {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
  struct CREDENTIAL { public int Flags, Type; public string TargetName, Comment;
    public long LastWritten; public int CredentialBlobSize; public IntPtr CredentialBlob;
    public int Persist, AttributeCount; public IntPtr Attributes; public string TargetAlias, UserName; }
  [DllImport("advapi32", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CredWriteW(ref CREDENTIAL c, int f);
  [DllImport("advapi32", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CredReadW(string t, int type, int f, out IntPtr c);
  [DllImport("advapi32", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CredDeleteW(string t, int type, int f);
  [DllImport("advapi32")] static extern void CredFree(IntPtr p);
  public static void Set(string t, string s) {
    var b = Encoding.UTF8.GetBytes(s);
    if (b.Length > 2560) throw new ArgumentException("secret too large (max 2560 bytes)");
    var h = Marshal.AllocHGlobal(Math.Max(b.Length, 1));
    try {
      Marshal.Copy(b, 0, h, b.Length);
      var c = new CREDENTIAL { Type = 1, TargetName = t, CredentialBlobSize = b.Length, CredentialBlob = h, Persist = 2, UserName = Environment.UserName };
      if (!CredWriteW(ref c, 0)) throw new System.ComponentModel.Win32Exception();
    } finally { Marshal.FreeHGlobal(h); }
  }
  public static string Get(string t) {
    IntPtr p; if (!CredReadW(t, 1, 0, out p)) throw new System.ComponentModel.Win32Exception();
    try { var c = (CREDENTIAL)Marshal.PtrToStructure(p, typeof(CREDENTIAL)); var b = new byte[c.CredentialBlobSize];
      Marshal.Copy(c.CredentialBlob, b, 0, b.Length); return Encoding.UTF8.GetString(b); } finally { CredFree(p); } }
  public static void Del(string t) { if (!CredDeleteW(t, 1, 0)) throw new System.ComponentModel.Win32Exception(); }
}
'@
$u = New-Object System.Text.UTF8Encoding $false; [Console]::InputEncoding = $u; [Console]::OutputEncoding = $u
try {
  switch ($Op) {
    'set' { [WinCred]::Set($Target, [Console]::In.ReadToEnd().TrimEnd("`r","`n")) }
    'get' { [Console]::Out.Write([WinCred]::Get($Target)) }
    'del' { [WinCred]::Del($Target) }
  }
} catch {
  # PowerShell wraps .NET exceptions (MethodInvocationException), so walk to the Win32Exception.
  $e = $_.Exception; while ($e -and $e -isnot [System.ComponentModel.Win32Exception] -and $e.InnerException) { $e = $e.InnerException }
  # CBOX_NOT_FOUND is a debug aid: Rust reads only the exit code (3).
  if ($e -is [System.ComponentModel.Win32Exception] -and $e.NativeErrorCode -eq 1168) { [Console]::Error.Write('CBOX_NOT_FOUND'); exit 3 }
  [Console]::Error.Write($e.Message); exit 1
}
