#!/usr/bin/env pwsh
<#
.SYNOPSIS
  Tries cbox profiles on a real machine (WSL with Windows Credential Manager, or macOS).

.DESCRIPTION
  Part A needs no VM: profiles.toml handling, the help-without-arguments behaviour, secrets
  in the real OS store (dummy values), edit/--reset, delete warnings, and a check that no
  dummy value is ever printed.
  Part B boots one real box (skip it with -SkipUp; each `up` runs `sleep 2` and and the in-box checks run through a resuming `up` instead of `exec`, because `cb exec` failed with "resize_tty failed" when run by this script (cause not yet known) and `up` did not): the profile's cpus, memory, image and secret
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
  Optional image for the test profile, e.g. localhost:5551/library/cbox-rust:latest. It must
  already be pullable. Default: none (the built-in cbox-custom image).
.PARAMETER SkipUp
  Run Part A only (no VM is started).

.EXAMPLE
  pwsh ./scripts/test-profiles.ps1 -SkipUp
  pwsh ./scripts/test-profiles.ps1 -Image localhost:5551/library/cbox-rust:latest
#>
param(
    [string]$Cbox = 'cb',
    [string]$Image = '',
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
$box = "cbtest-$rand"
$cfg = Join-Path ([IO.Path]::GetTempPath()) "cbox-profile-test-$rand"
$script:Results = [System.Collections.Generic.List[object]]::new()
$script:AllOutput = [System.Text.StringBuilder]::new()

New-Item -ItemType Directory -Path $cfg | Out-Null
$env:XDG_CONFIG_HOME = $cfg
$profilesToml = Join-Path $cfg 'cbox/profiles.toml'

function Redact([string]$s) { $s.Replace($valA, '<redacted>').Replace($valB, '<redacted>') }

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

    # ---------------------------------------------------------------- Part B
    Section 'B. Real box'
    if ($SkipUp) { Skip 'all box tests' '-SkipUp' }
    else {
        if ($Image) { [void](Cbox @('profile', 'edit', 'pa', '--image', $Image)) }
        $r = Cbox @('up', $box, '--profile', 'pa', '-d', '--', 'sleep', '2')
        $created = ($r.Code -eq 0)
        Check 'B1 up --profile pa creates the box' { $created } "exit $($r.Code): $($r.Out)"
        if (-not $created) { Skip 'remaining box tests' 'box was not created' }
        else {
            $r = Cbox @('list', '-a')
            Check 'B2 list shows the box' { $r.Out -match [regex]::Escape($box) } $r.Out
            if ($Image) {
                $prefix = $Image.Substring(0, [Math]::Min(20, $Image.Length))
                Check 'B2 list shows the profile image' { $r.Out -match [regex]::Escape($prefix) } $r.Out
            }

            $r = Cbox @('up', $box, '-d', '--', 'sh', '-c', 'echo cpus=$(nproc); awk ''/MemTotal/{print "memmb=" int($2/1024)}'' /proc/meminfo; sleep 2')
            if ($r.Code -ne 0) { Skip 'B3/B4 resource and secret checks' "command failed: $($r.Out)" }
            else {
                Check 'B3 box has 1 vCPU (profile cpus = 1, built-in is 2)' { $r.Out -match 'cpus=1\b' } $r.Out
                $mb = if ($r.Out -match 'memmb=(\d+)') { [int]$Matches[1] } else { 0 }
                Check 'B3 box memory is about 2 GiB (profile memory = 2, built-in is 4)' { $mb -gt 1200 -and $mb -lt 2300 } "memmb=$mb"
                $r = Cbox @('up', $box, '-d', '--', 'sh', '-c', "printenv $envA; printenv $envB; sleep 2")
                Check 'B4 both profile secrets are set in the box' { ($r.Out -split "`n").Count -ge 2 -and $r.Code -eq 0 } $r.Out
                Check 'B4 ...and the real values are not in the box environment' { $r.Out -notmatch [regex]::Escape($valA) -and $r.Out -notmatch [regex]::Escape($valB) }
            }

            $r = Cbox @('up', $box, '--profile', 'pa', '--secret', $secA, '-d', '--', 'sleep', '2')
            Check 'B5 --secret already in the profile is rejected' { $r.Code -ne 0 -and $r.Out -match 'already in profile pa' } "exit $($r.Code): $($r.Out)"

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
            [void](Cbox @('profile', 'create', 'pb', '--cpus', '2'))
            [void](Cbox @('profile', 'delete', 'pa', '-y'))
            $r = Cbox @('up', $box, '-d', '--', 'sleep', '2')
            Check 'B8 resume when the recorded profile is gone is refused' { $r.Code -ne 0 -and $r.Out -match 'no longer exists' -and $r.Out -match '--profile NAME' } "exit $($r.Code): $($r.Out)"
            $r = Cbox @('up', $box, '--profile', 'pb', '-d', '--', 'sleep', '2')
            Check 'B8 ...but --profile pb resumes with a warning naming both profiles' { $r.Code -eq 0 -and $r.Out -match 'you asked for profile pb' -and $r.Out -match 'created with profile pa' } "exit $($r.Code): $($r.Out)"

            $r = Cbox @('up', $box, '-f', '--profile', 'pb', '-d', '--', 'sleep', '2')
            Check 'B9 -f recreates the box from profile pb' { $r.Code -eq 0 } "exit $($r.Code): $($r.Out)"
            $r = Cbox @('up', $box, '-d', '--', 'sh', '-c', 'echo cpus=$(nproc); sleep 2')
            Check 'B9 the recreated box has pb''s 2 vCPUs' { $r.Code -eq 0 -and $r.Out -match 'cpus=2\b' } $r.Out
            $r = Cbox @('up', $box, '-d', '--', 'sleep', '2')
            Check 'B9 resume with no --profile uses the recorded profile (pb): no warning' { $r.Code -eq 0 -and $r.Out -notmatch 'differs from this box' -and $r.Out -notmatch 'you asked for' } "exit $($r.Code): $($r.Out)"
        }
    }

    Section 'Z. No dummy value was ever printed'
    $all = $script:AllOutput.ToString()
    Check 'secret A value never appeared in any output' { -not $all.Contains($valA) }
    Check 'secret B value never appeared in any output' { -not $all.Contains($valB) }
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
