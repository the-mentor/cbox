#!/usr/bin/env pwsh
<#
.SYNOPSIS
  Tries cbox profiles on a real machine (WSL with Windows Credential Manager, or macOS).

.DESCRIPTION
  Part A needs no VM: profiles.toml handling, the help-without-arguments behaviour, secrets
  in the real OS store (dummy values), edit/--reset, delete warnings, and a check that no
  dummy value is ever printed.
  Part B boots one real box (skip it with -SkipUp; each `up` runs `sleep 2` and and the in-box checks start the cbox binary directly through .NET: build it first with `cb build-cbox`): the profile's cpus, memory, image and secret
  reach the box, the "already in profile" check, and the resume cases (diff warning, deleted
  secret, deleted profile, -f).

  Safe by design:
    * XDG_CONFIG_HOME points at a temp folder, so your real profiles.toml and secrets.json are
      never read or written.
    * Secrets are named cbtest-<random> with dummy values, and are deleted again at the end.
    * The box is named cbtest-<random> and is removed with `cbox down` at the end.
    * Secret values are never printed; every output is scanned for them.

.PARAMETER Cbox
  The command that runs cbox. Default: cb (the just wrapper, so every call goes through the
  justfile recipes: profile, secret, up, exec, list, down). A direct cbox binary also works,
  e.g. ./cbox/target/release/cbox. Note that `cb --help` and `cb --version` are just's own, so
  the script never uses them.
.PARAMETER Image
  Image for the test profile. It must be pullable; the first run pulls it, which can take a while.
  Default: ghcr.io/the-mentor/cbox-base:latest. Pass -Image '' to skip the image check.
.PARAMETER SkipUp
  Run Part A only (no VM is started).

.EXAMPLE
  pwsh ./scripts/e2e/profiles.ps1 -SkipUp
  pwsh ./scripts/e2e/profiles.ps1 -Image localhost:5551/library/cbox-rust:latest
  pwsh ./scripts/e2e/profiles.ps1 -Image ''
#>
param(
    [string]$Cbox = 'cb',
    [string]$Image = 'ghcr.io/the-mentor/cbox-base:latest',
    [switch]$SkipUp
)

$ErrorActionPreference = 'Continue'
$script:Exe = (Get-Command $Cbox -ErrorAction Stop).Source
$rand = -join ((48..57) + (97..102) | Get-Random -Count 6 | ForEach-Object { [char]$_ })
$secA = "cbtest-a-$rand"
$secB = "cbtest-b-$rand"
$envA = 'CBTEST_KEY_A'
$envB = 'CBTEST_KEY_B'
$valA = "DUMMY-VALUE-A-$rand"
$valB = "DUMMY-VALUE-B-$rand"
$valTok = "DUMMY-VALUE-TOK-$rand"
$box = "cbtest-$rand"
$cfg = Join-Path ([IO.Path]::GetTempPath()) "cbox-profile-test-$rand"
$script:Results = [System.Collections.Generic.List[object]]::new()
$script:AllOutput = [System.Text.StringBuilder]::new()

New-Item -ItemType Directory -Path $cfg | Out-Null
$env:XDG_CONFIG_HOME = $cfg
$profilesToml = Join-Path $cfg 'cbox/profiles.toml'

function Redact([string]$s) { $s.Replace($valA, '<redacted>').Replace($valB, '<redacted>').Replace($valTok, '<redacted>') }

# In-box checks run the cbox binary directly, not through `cb`: just pastes its arguments unquoted
# into a shell script, so a guest command like `sh -c 'a; b'` is split on the `;` and the box runs
# only `sh -c a`, which exits at once. They also start the process through .NET so that stdin stays
# open until cbox exits: cbox ends the session (and can drop the guest's output) when stdin hits EOF.
$script:DirectBin = Join-Path $PSScriptRoot '../../cbox/target/release/cbox'
$script:Registries = Join-Path $PSScriptRoot '../../registries.local.json'
function InBox([string]$GuestCmd) {
    $psi = [System.Diagnostics.ProcessStartInfo]::new($script:DirectBin)
    foreach ($a in @('up', '--config', $script:Registries, $box, '-d', '--', 'sh', '-c', $GuestCmd)) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    # Read both streams while the process runs, or a full pipe would block it.
    $stdout = $p.StandardOutput.ReadToEndAsync()
    $stderr = $p.StandardError.ReadToEndAsync()
    $timedOut = -not $p.WaitForExit(120000)
    if ($timedOut) { $p.Kill($true); $p.WaitForExit() }
    try { $p.StandardInput.Close() } catch { }   # the process may be gone already
    $text = ($stderr.Result + $stdout.Result).TrimEnd()
    if ($timedOut) { $text += "`n(timed out after 120 s and was killed)" }
    $r = [pscustomobject]@{ Out = $text; Code = $(if ($timedOut) { -1 } else { $p.ExitCode }) }
    [void]$script:AllOutput.AppendLine($r.Out)
    $r
}
# Runs cbox, returns Out (stdout+stderr) and Code. Optional $In is piped to stdin.
function Cbox {
    param([string[]]$A, [string]$In = $null)
    $o = if ($null -ne $In) { $In | & $script:Exe @A 2>&1 } else { & $script:Exe @A 2>&1 }
    $r = [pscustomobject]@{ Out = (($o | Out-String).TrimEnd()); Code = $LASTEXITCODE }
    [void]$script:AllOutput.AppendLine($r.Out)
    $r
}

function Check {
    param([string]$Name, [scriptblock]$Test, [string]$Detail = '')
    $ok = $false
    try { $ok = [bool](& $Test) } catch { $Detail = "exception: $($_.Exception.Message)" }
    $script:Results.Add([pscustomobject]@{ Name = $Name; Ok = $ok })
    if ($ok) { Write-Host "  PASS  $Name" -ForegroundColor Green }
    else {
        Write-Host "  FAIL  $Name" -ForegroundColor Red
        if ($Detail) { Write-Host (Redact $Detail) -ForegroundColor DarkGray }
    }
}

function Section([string]$t) { Write-Host "`n== $t" -ForegroundColor Cyan }
function Skip([string]$Name, [string]$Why) {
    $script:Results.Add([pscustomobject]@{ Name = $Name; Ok = $null })
    Write-Host "  SKIP  $Name ($Why)" -ForegroundColor Yellow
}

try {
    # Which version of the repo this run tested, so a pasted result says what it ran against.
    $commit = (& git -C $PSScriptRoot log -1 --format='%h %s (%cd)' --date=short 2>$null)
    $dirty = if (& git -C $PSScriptRoot status --porcelain --untracked-files=no 2>$null) { ' + uncommitted changes' } else { '' }
    Write-Host "commit: $commit$dirty"
    Write-Host "running cbox through: $script:Exe"
    Write-Host "temp config dir: $cfg   test secrets: $secA, $secB   test box: $box"

    # ---------------------------------------------------------------- Part A
    Section 'A1. First run creates profiles.toml'
    Check 'file does not exist yet' { -not (Test-Path $profilesToml) }
    # Bare `profile` prints help and exits before doing any work, so it proves the file is made
    # on the very first run, even for a command that only shows help.
    $r = Cbox @('profile')
    Check 'first run (`profile`, help only) exits 0' { $r.Code -eq 0 } $r.Out
    $body = Get-Content $profilesToml -Raw -ErrorAction SilentlyContinue
    Check 'profiles.toml now exists with an empty [default]' { $null -ne $body -and $body.Trim() -eq '[default]' } "file content: $body"
    $before = $body
    [void](Cbox @('profile', 'list'))
    Check 'a second run leaves the file untouched' { (Get-Content $profilesToml -Raw -ErrorAction SilentlyContinue) -eq $before }
    $r = Cbox @('profile', 'list')
    Check '`profile list` shows default (empty)' { $r.Code -eq 0 -and $r.Out -match 'default' -and $r.Out -match '\(empty\)' } $r.Out
    $r = Cbox @('profile', 'validate')
    Check '`profile validate` passes' { $r.Code -eq 0 -and $r.Out -match 'valid' } $r.Out

    Section 'A2. Commands without arguments print help and exit 0'
    foreach ($c in @(@('profile'), @('profile', 'create'), @('profile', 'edit'), @('profile', 'delete'),
                     @('secret'), @('secret', 'create'), @('secret', 'edit'), @('secret', 'delete'))) {
        $r = Cbox $c
        # secret create/edit/delete may still print an error: that needs the separate secret-help change.
        Check "cbox $($c -join ' ') -> help, exit 0" { $r.Code -eq 0 -and $r.Out -match 'Usage:' } "exit $($r.Code): $($r.Out)"
    }
    $r = Cbox @('profile', 'bogus')
    Check 'a real usage error still exits 2' { $r.Code -eq 2 } "exit $($r.Code)"

    Section 'A3. Store two dummy secrets in the real OS store'
    $r = Cbox @('secret', 'create', $secA, '--host', 'example.com', '--env', $envA) -In $valA
    Check "secret create $secA" { $r.Code -eq 0 } $r.Out
    $r = Cbox @('secret', 'create', $secB, '--host', 'example.com', '--env', $envB) -In $valB
    Check "secret create $secB" { $r.Code -eq 0 } $r.Out
    $r = Cbox @('secret', 'list')
    Check 'secret list shows both names' { $r.Out -match $secA -and $r.Out -match $secB } $r.Out

    Section 'A4. profile create / validation'
    $r = Cbox @('profile', 'create', 'pa', '--secrets', "$secA,$secB", '--cpus', '1', '--memory', '2')
    Check 'create pa with two secrets, cpus 1, memory 2' { $r.Code -eq 0 } $r.Out
    $r = Cbox @('profile', 'create', 'pa')
    Check 'create of an existing name fails' { $r.Code -ne 0 } $r.Out
    $r = Cbox @('profile', 'create', 'bad1', '--secrets', "no-such-$rand")
    Check 'create with an unknown secret fails' { $r.Code -ne 0 } $r.Out
    Check '  ...and writes nothing' { (Get-Content $profilesToml -Raw -ErrorAction SilentlyContinue) -notmatch 'bad1' }
    $r = Cbox @('profile', 'create', 'bad2', '--secrets', 'gh')
    Check 'create with the reserved name gh fails' { $r.Code -ne 0 } $r.Out
    $r = Cbox @('profile', 'create', 'bad3', '--cpus', '0')
    Check 'create with cpus 0 fails' { $r.Code -ne 0 } $r.Out
    $r = Cbox @('profile', 'list')
    Check 'list shows pa with both secrets, cpus 1, memory 2' { $r.Out -match 'pa' -and $r.Out -match $secA -and $r.Out -match 'cpus: 1' -and $r.Out -match 'memory: 2' } $r.Out

    Section 'A5. profile edit and --reset'
    $r = Cbox @('profile', 'edit', 'pa', '--remove-secrets', $secB)
    Check 'edit --remove-secrets' { $r.Code -eq 0 } $r.Out
    $r = Cbox @('profile', 'edit', 'pa', '--add-secrets', $secB)
    Check 'edit --add-secrets' { $r.Code -eq 0 } $r.Out
    $r = Cbox @('profile', 'edit', 'pa', '--add-secrets', $secB)
    Check 'adding a secret twice fails' { $r.Code -ne 0 } $r.Out
    $r = Cbox @('profile', 'edit', 'pa', '--secrets', $secA, '--add-secrets', $secB)
    Check '--secrets together with --add-secrets is rejected' { $r.Code -ne 0 } $r.Out
    $r = Cbox @('profile', 'edit', 'pa', '--cpus', '3', '--reset', 'cpus')
    Check '--reset cpus together with --cpus is rejected' { $r.Code -ne 0 } $r.Out
    $r = Cbox @('profile', 'edit', 'pa', '--reset', 'bogus')
    Check '--reset with an unknown field is rejected' { $r.Code -ne 0 } $r.Out
    [void](Cbox @('profile', 'create', 'pr', '--secrets', $secA, '--cpus', '2', '--memory', '3', '--disk-size', '12', '--image', 'some/image:tag'))
    $r = Cbox @('profile', 'edit', 'pr', '--reset', 'secrets,image,cpus,memory,disk-size')
    Check '--reset clears every field' { $r.Code -eq 0 } $r.Out
    $r = Cbox @('profile', 'list')
    Check '  ...pr is empty afterwards' { ($r.Out -split "`n" | Select-String -Pattern '^pr' -Context 0, 1).Context.PostContext -match '\(empty\)' } $r.Out
    # Reset ONE field: the others must stay. (A bug that clears too much would pass the all-fields check above.)
    function ProfileBlock([string]$Out, [string]$Name) {
        [regex]::Match($Out, "(?m)^$Name\r?\n((?:[ ]{2}.*(?:\r?\n|$))*)").Groups[1].Value
    }
    [void](Cbox @('profile', 'create', 'po', '--secrets', $secA, '--cpus', '2', '--memory', '3', '--disk-size', '12', '--image', 'some/image:tag'))
    $r = Cbox @('profile', 'edit', 'po', '--reset', 'image')
    Check '--reset image succeeds' { $r.Code -eq 0 } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'po'
    Check '  ...image is gone' { $blk -notmatch 'image:' } $blk
    Check '  ...secret, cpus, memory and disk size are kept' { $blk -match [regex]::Escape($secA) -and $blk -match 'cpus: 2' -and $blk -match 'memory: 3' -and $blk -match 'disk size: 12' } $blk
    $r = Cbox @('profile', 'edit', 'po', '--reset', 'cpus,memory', '--image', 'other/image:1', '--disk-size', '15')
    Check '--reset two fields while setting two others succeeds' { $r.Code -eq 0 } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'po'
    Check '  ...cpus and memory are gone' { $blk -notmatch 'cpus:' -and $blk -notmatch 'memory:' } $blk
    Check '  ...the new image and disk size are set, the secret is kept' { $blk -match 'image: other/image:1' -and $blk -match 'disk size: 15' -and $blk -match [regex]::Escape($secA) } $blk
    $r = Cbox @('profile', 'edit', 'po', '--reset', 'secrets', '--add-secrets', $secB)
    Check '--reset secrets together with --add-secrets is rejected' { $r.Code -ne 0 } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'po'
    Check '  ...and the rejected edit changed nothing' { $blk -match [regex]::Escape($secA) -and $blk -match 'image: other/image:1' } $blk
    $r = Cbox @('profile', 'edit', 'po', '--reset', 'secrets', '--secrets', $secB)
    Check '--reset secrets together with --secrets is rejected' { $r.Code -ne 0 } $r.Out
    $r = Cbox @('profile', 'edit', 'po', '--reset', 'secrets', '--remove-secrets', $secA)
    Check '--reset secrets together with --remove-secrets is rejected' { $r.Code -ne 0 } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'po'
    Check '  ...and the rejected edits changed nothing' { $blk -match [regex]::Escape($secA) -and $blk -notmatch [regex]::Escape($secB) } $blk
    $r = Cbox @('profile', 'edit', 'po', '--reset', 'secrets')
    Check '--reset secrets alone empties the list and keeps the other fields' { $r.Code -eq 0 -and (ProfileBlock (Cbox @('profile', 'list')).Out 'po') -notmatch [regex]::Escape($secA) -and (ProfileBlock (Cbox @('profile', 'list')).Out 'po') -match 'image: other/image:1' } $r.Out
    $r = Cbox @('profile', 'edit', 'po', '--reset', 'image', '--cpus', '4')
    Check '--reset image together with --cpus 4 succeeds (different fields do not conflict)' { $r.Code -eq 0 } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'po'
    Check '  ...the image is gone, cpus is 4, disk size is kept' { $blk -notmatch 'image:' -and $blk -match 'cpus: 4' -and $blk -match 'disk size: 15' } $blk
    [void](Cbox @('profile', 'delete', 'po', '-y'))
    $r = Cbox @('profile', 'delete', 'default', '-y')
    Check 'deleting default is refused' { $r.Code -ne 0 } $r.Out
    $r = Cbox @('profile', 'delete', 'pr', '-y')
    Check 'delete pr -y' { $r.Code -eq 0 } $r.Out

    Section 'A6. secret delete warns about profiles that use the secret'
    [void](Cbox @('profile', 'create', 'pw', '--secrets', $secB))
    # Create a throwaway third secret so the two main ones stay available for Part B.
    $secC = "cbtest-c-$rand"
    [void](Cbox @('secret', 'create', $secC, '--host', 'example.com', '--env', 'CBTEST_KEY_C') -In "DUMMY-VALUE-C-$rand")
    [void](Cbox @('profile', 'create', 'pc', '--secrets', $secC))
    $r = Cbox @('secret', 'delete', $secC, '-y')
    Check 'delete warns and names profile pc' { $r.Out -match 'used by profile' -and $r.Out -match '\bpc\b' } $r.Out
    Check '  ...and still deletes (exit 0)' { $r.Code -eq 0 } $r.Out
    $r = Cbox @('profile', 'validate', 'pc')
    Check 'validate pc now reports the secret by position, not by name' { $r.Code -ne 0 -and $r.Out -match 'entry 1 is not a stored secret' -and $r.Out -notmatch $secC } $r.Out
    $r = Cbox @('profile', 'validate', 'pa')
    Check 'validate pa (only that profile) still passes' { $r.Code -eq 0 } $r.Out
    [void](Cbox @('profile', 'delete', 'pc', '-y'))
    [void](Cbox @('profile', 'delete', 'pw', '-y'))

    Section 'A7. A pasted value in profiles.toml is never echoed'
    $good = Get-Content $profilesToml -Raw -ErrorAction SilentlyContinue
    $pasted = "SECRET-PASTED-$rand"
    Set-Content $profilesToml ($good + "`n[leak1]`ntoken = `"$pasted`"`n")
    $r = Cbox @('profile', 'validate')
    Check 'unknown key: fails, names file and line, hides the value' { $r.Code -ne 0 -and $r.Out -match 'line \d+' -and $r.Out -notmatch $pasted } $r.Out
    Set-Content $profilesToml ($good + "`n[leak2]`nsecrets = [`"$pasted`"]`n")
    $r = Cbox @('profile', 'validate')
    Check 'bad secrets entry: reported by position, value hidden' { $r.Code -ne 0 -and $r.Out -match 'entry 1' -and $r.Out -notmatch $pasted } $r.Out
    Set-Content $profilesToml ($good + "`n[leak3]`ncpus = `"$pasted`"`n")
    $r = Cbox @('profile', 'validate')
    Check 'wrong type: value hidden' { $r.Code -ne 0 -and $r.Out -notmatch $pasted } $r.Out
    Set-Content $profilesToml $good

    Section 'A8. Environment variables'
    $gitEnv = @('--env', 'GIT_AUTHOR_NAME=Avri Chen-Roth', '--env', 'GIT_AUTHOR_EMAIL=11185446+the-mentor@users.noreply.github.com',
                '--env', 'GIT_COMMITTER_NAME=Avri Chen-Roth', '--env', 'GIT_COMMITTER_EMAIL=11185446+the-mentor@users.noreply.github.com')
    $r = Cbox (@('profile', 'create', 'pe') + $gitEnv)
    Check 'create pe with the four git variables' { $r.Code -eq 0 -and $r.Out -match 'Created profile "pe"' } $r.Out
    Check '  ...no credential warning for them' { $r.Out -notmatch 'looks like a credential' } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'pe'
    Check 'list shows the four variables with their values' { $blk -match 'env: GIT_AUTHOR_NAME=Avri Chen-Roth' -and $blk -match 'env: GIT_AUTHOR_EMAIL=11185446\+the-mentor@users\.noreply\.github\.com' -and $blk -match 'env: GIT_COMMITTER_NAME=' -and $blk -match 'env: GIT_COMMITTER_EMAIL=' } $blk
    $r = Cbox @('profile', 'validate', 'pe')
    Check 'validate pe passes without a credential warning' { $r.Code -eq 0 -and $r.Out -notmatch 'looks like a credential' } $r.Out

    $r = Cbox @('profile', 'create', 'pt', '--env', "MY_API_TOKEN=$valTok")
    Check 'create pt with a credential-looking variable succeeds' { $r.Code -eq 0 -and $r.Out -match 'Created profile "pt"' } $r.Out
    Check '  ...warns, naming MY_API_TOKEN' { $r.Out -match 'looks like a credential' -and $r.Out -match 'MY_API_TOKEN' } $r.Out
    Check '  ...and never prints the value' { -not $r.Out.Contains($valTok) } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'pt'
    Check 'list hides it: MY_API_TOKEN=<hidden>' { $blk -match 'env: MY_API_TOKEN=<hidden>' -and -not $blk.Contains($valTok) } $blk
    [void](Cbox @('profile', 'delete', 'pt', '-y'))

    $r = Cbox @('profile', 'edit', 'pe', '--env', 'GIT_AUTHOR_NAME=Someone Else')
    Check 'edit --env replaces one value' { $r.Code -eq 0 -and $r.Out -match 'Updated profile "pe"' } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'pe'
    Check '  ...the new value is set and the other three are kept' { $blk -match 'env: GIT_AUTHOR_NAME=Someone Else' -and $blk -notmatch 'GIT_AUTHOR_NAME=Avri' -and $blk -match 'env: GIT_AUTHOR_EMAIL=' -and $blk -match 'env: GIT_COMMITTER_NAME=Avri Chen-Roth' -and $blk -match 'env: GIT_COMMITTER_EMAIL=' } $blk
    $r = Cbox @('profile', 'edit', 'pe', '--unset-env', 'GIT_COMMITTER_EMAIL')
    Check 'edit --unset-env removes one variable' { $r.Code -eq 0 } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'pe'
    Check '  ...it is gone and the others stay' { $blk -notmatch 'GIT_COMMITTER_EMAIL' -and $blk -match 'GIT_AUTHOR_NAME' -and $blk -match 'GIT_COMMITTER_NAME' } $blk
    $r = Cbox @('profile', 'edit', 'pe', '--unset-env', 'NO_SUCH_VAR')
    Check '--unset-env of an unknown name fails' { $r.Code -ne 0 -and $r.Out -match 'has no env variable' } $r.Out
    $r = Cbox @('profile', 'edit', 'pe', '--env', 'X=1', '--unset-env', 'X')
    Check '--env X=1 together with --unset-env X is rejected' { $r.Code -ne 0 } $r.Out
    $r = Cbox @('profile', 'edit', 'pe', '--reset', 'env', '--env', 'X=1')
    Check '--reset env together with --env is rejected' { $r.Code -ne 0 } $r.Out
    $blk = ProfileBlock (Cbox @('profile', 'list')).Out 'pe'
    Check '  ...and the rejected edits changed nothing' { $blk -match 'GIT_AUTHOR_NAME=Someone Else' -and $blk -notmatch 'X=1' } $blk
    $r = Cbox @('profile', 'edit', 'pe', '--reset', 'env')
    Check '--reset env clears every variable' { $r.Code -eq 0 -and (ProfileBlock (Cbox @('profile', 'list')).Out 'pe') -match '\(empty\)' } $r.Out
    [void](Cbox @('profile', 'delete', 'pe', '-y'))

    # Hand-edited profiles.toml, like A7.
    $good = Get-Content $profilesToml -Raw -ErrorAction SilentlyContinue
    Set-Content $profilesToml ($good + "`n[envdup]`nenv = [`"A=1`", `"A=2`"]`n")
    $r = Cbox @('profile', 'validate')
    Check 'duplicate env name: validate fails with "is set twice"' { $r.Code -ne 0 -and $r.Out -match 'is set twice' } $r.Out
    $noEq = "NOEQUALS-$rand"
    Set-Content $profilesToml ($good + "`n[envbad]`nenv = [`"$noEq`"]`n")
    $r = Cbox @('profile', 'validate')
    Check 'env entry without "=": reported by position, value hidden' { $r.Code -ne 0 -and $r.Out -match 'env entry 1' -and $r.Out -notmatch $noEq } $r.Out
    Set-Content $profilesToml $good

    # ---------------------------------------------------------------- Part B
    Section 'B. Real box'
    if ($SkipUp) { Skip 'all box tests' '-SkipUp' }
    else {
        if ($Image) { [void](Cbox @('profile', 'edit', 'pa', '--image', $Image)) }
        [void](Cbox @('profile', 'edit', 'pa', '--env', 'CBT_PLAIN=hello', '--env', 'CBT_SPACED=a b=c'))
        $r = Cbox @('up', $box, '--profile', 'pa', '-d', '--', 'sleep', '2')
        $created = ($r.Code -eq 0)
        Check 'B1 up --profile pa creates the box' { $created } "exit $($r.Code): $($r.Out)"
        if (-not $created) { Skip 'remaining box tests' 'box was not created' }
        else {
            $r = Cbox @('list', '-a')
            Check 'B2 list shows the box' { $r.Out -match [regex]::Escape($box) } $r.Out
            # Look at this box's own row: other boxes on the machine may share the image.
            $row = ($r.Out -split "`n" | Where-Object { $_ -match [regex]::Escape($box) } | Select-Object -First 1)
            if ($Image) {
                # `list` truncates long image names, so compare the first 20 characters.
                $prefix = $Image.Substring(0, [Math]::Min(20, $Image.Length))
                Check 'B2 the box row shows the profile''s image' { $row -match [regex]::Escape($prefix) } $row
            }
            else { Skip 'B2 the box row shows the profile''s image' '-Image was empty' }

            $r = InBox 'echo cpus=$(nproc); awk ''/MemTotal/{print "memmb=" int($2/1024)}'' /proc/meminfo; sleep 2'
            if ($r.Code -ne 0) { Skip 'B3/B4 resource and secret checks' "command failed: $($r.Out)" }
            else {
                Check 'B3 box has 1 vCPU (profile cpus = 1, built-in is 2)' { $r.Out -match 'cpus=1\b' } $r.Out
                $mb = if ($r.Out -match 'memmb=(\d+)') { [int]$Matches[1] } else { 0 }
                Check 'B3 box memory is about 2 GiB (profile memory = 2, built-in is 4)' { $mb -gt 1200 -and $mb -lt 2300 } "memmb=$mb"
                $r = InBox "echo A=`$$envA; echo B=`$$envB; sleep 2"
                Check 'B4 both profile secrets are set in the box (non-empty)' { $r.Out -match '(?m)^A=\S+' -and $r.Out -match '(?m)^B=\S+' } $r.Out
                Check 'B4 ...and the real values are not in the box environment' { $r.Out -notmatch [regex]::Escape($valA) -and $r.Out -notmatch [regex]::Escape($valB) }
            }

            $r = InBox 'echo "P=$CBT_PLAIN"; echo "S=$CBT_SPACED"; sleep 2'
            Check 'B4 profile env reaches the box with exact values (spaces and "=" kept)' { $r.Code -eq 0 -and $r.Out -match '(?m)^P=hello\r?$' -and $r.Out -match '(?m)^S=a b=c\r?$' } $r.Out

            $r = Cbox @('up', $box, '--profile', 'pa', '--secret', $secA, '-d', '--', 'sleep', '2')
            Check 'B5 --secret already in the profile is rejected' { $r.Code -ne 0 -and $r.Out -match 'already in profile pa' } "exit $($r.Code): $($r.Out)"

            [void](Cbox @('profile', 'edit', 'pa', '--env', 'CBT_PLAIN=changed-value'))
            $r = Cbox @('up', $box, '-d', '--', 'sleep', '2')
            Check 'B5b resume after an env change names the variable but not the new value' { $r.Code -eq 0 -and $r.Out -match '(?m)env changed: .*CBT_PLAIN' -and $r.Out -notmatch 'changed-value' } "exit $($r.Code): $($r.Out)"

            [void](Cbox @('profile', 'edit', 'pa', '--cpus', '2'))
            $r = Cbox @('up', $box, '-d', '--', 'sleep', '2')
            Check 'B6 resume (no --profile) succeeds' { $r.Code -eq 0 } "exit $($r.Code): $($r.Out)"
            Check 'B6 ...prints the profile diff and points to -f' { $r.Out -match 'differs from this box' -and $r.Out -match 'cpus' -and $r.Out -match '-f/--force' } $r.Out

            $r = Cbox @('secret', 'delete', $secB, '-y')
            Check 'B7 secret delete names profile pa' { $r.Out -match 'used by profile' -and $r.Out -match '\bpa\b' } $r.Out
            $r = Cbox @('up', $box, '-d', '--', 'sleep', '2')
            Check 'B7 resume with a deleted secret warns but still works' { $r.Code -eq 0 -and $r.Out -match 'warning' -and $r.Out -match 'not a stored secret' } "exit $($r.Code): $($r.Out)"
            $r = Cbox @('up', "$box-new", '--profile', 'pa', '-d', '--', 'sleep', '2')
            Check 'B7 a NEW box from that profile fails validation' { $r.Code -ne 0 -and $r.Out -match 'not a stored secret' } "exit $($r.Code): $($r.Out)"

            [void](Cbox @('profile', 'edit', 'pa', '--remove-secrets', $secB))
            [void](Cbox @('profile', 'create', 'pb', '--cpus', '3'))
            [void](Cbox @('profile', 'delete', 'pa', '-y'))
            $r = Cbox @('up', $box, '-d', '--', 'sleep', '2')
            Check 'B8 resume when the recorded profile is gone is refused' { $r.Code -ne 0 -and $r.Out -match 'no longer exists' -and $r.Out -match '--profile NAME' } "exit $($r.Code): $($r.Out)"
            $r = Cbox @('up', $box, '--profile', 'pb', '-d', '--', 'sleep', '2')
            Check 'B8 ...but --profile pb resumes with a warning naming both profiles' { $r.Code -eq 0 -and $r.Out -match 'you asked for profile pb' -and $r.Out -match 'created with profile pa' } "exit $($r.Code): $($r.Out)"

            $r = Cbox @('up', $box, '-f', '--profile', 'pb', '-d', '--', 'sleep', '2')
            Check 'B9 -f recreates the box from profile pb' { $r.Code -eq 0 } "exit $($r.Code): $($r.Out)"
            $r = Cbox @('list', '-a')
            $row = ($r.Out -split "`n" | Where-Object { $_ -match [regex]::Escape($box) } | Select-Object -First 1)
            Check 'B9 pb sets no image, so the recreated box uses the built-in cbox-custom' { $row -match 'cbox-custom' } $row
            $r = InBox 'echo cpus=$(nproc); sleep 2'
            Check 'B9 the recreated box has pb''s 3 vCPUs (built-in is 2)' { $r.Code -eq 0 -and $r.Out -match 'cpus=3\b' } $r.Out
            $r = Cbox @('up', $box, '-d', '--', 'sleep', '2')
            Check 'B9 resume with no --profile uses the recorded profile (pb): no warning' { $r.Code -eq 0 -and $r.Out -notmatch 'differs from this box' -and $r.Out -notmatch 'you asked for' } "exit $($r.Code): $($r.Out)"
        }
    }

    Section 'Z. No dummy value was ever printed'
    $all = $script:AllOutput.ToString()
    Check 'secret A value never appeared in any output' { -not $all.Contains($valA) }
    Check 'secret B value never appeared in any output' { -not $all.Contains($valB) }
    Check 'credential-looking env value never appeared in any output' { -not $all.Contains($valTok) }
}
finally {
    Section 'Cleanup'
    if (-not $SkipUp) { $r = Cbox @('down', $box); Write-Host "  down $box -> exit $($r.Code)"; [void](Cbox @('down', "$box-new")) }
    foreach ($s in @($secA, $secB, "cbtest-c-$rand")) { [void](Cbox @('secret', 'delete', $s, '-y')) }
    $r = Cbox @('secret', 'list')
    if ($r.Out -match "cbtest-.*-$rand") { Write-Host "  WARNING: test secrets may remain in the OS store: $($r.Out)" -ForegroundColor Yellow }
    else { Write-Host '  test secrets removed from the OS store' }
    Remove-Item -Recurse -Force $cfg -ErrorAction SilentlyContinue
    Remove-Item Env:XDG_CONFIG_HOME -ErrorAction SilentlyContinue
}

$pass = ($script:Results | Where-Object { $_.Ok -eq $true }).Count
$fail = ($script:Results | Where-Object { $_.Ok -eq $false }).Count
$skip = ($script:Results | Where-Object { $null -eq $_.Ok }).Count
Write-Host "`n$pass passed, $fail failed, $skip skipped" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { $script:Results | Where-Object { $_.Ok -eq $false } | ForEach-Object { Write-Host "  FAILED: $($_.Name)" -ForegroundColor Red } }
exit ([int][bool]$fail)
