# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#Requires -Version 7.0
[CmdletBinding(DefaultParameterSetName = 'Build')]
param(
    [Parameter(Mandatory, ParameterSetName = 'Setup')][switch] $Setup,
    [Parameter(ParameterSetName = 'Build')]
    [ValidateSet('all', 'core', 'web', 'sentinel', 'touch')][string] $Component = 'all'
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

# Keep these pins aligned with onboard/build.Dockerfile.
$rustVersion = '1.99.0'
$toolchain = "$rustVersion-x86_64-pc-windows-msvc"
$target = 'aarch64-unknown-linux-musl'
$zig = @{
    Name = 'zig'; Version = '0.17.0'; VersionArgs = @('version'); VersionPrefix = ''
    Url = 'https://ziglang.org/download/0.17.0/zig-x86_64-windows-0.17.0.zip'
    Sha256 = 'b5663f69581dcf391293fbf16c06cb80d81d806545ce618b4d0bab7f0eb8c428'
}
$zigbuild = @{
    Name = 'cargo-zigbuild'; Version = '0.23.4'; VersionArgs = @('--version'); VersionPrefix = 'cargo-zigbuild '
    Url = 'https://github.com/rust-cross/cargo-zigbuild/releases/download/v0.23.4/cargo-zigbuild-x86_64-pc-windows-msvc.zip'
    Sha256 = 'cd1226091f9f99ac7b46fb413968fb0b46edbe3ea961817d31b968352de4d4a6'
}

function Invoke-Native([string] $Exe, [string[]] $Arguments) {
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Exit ${LASTEXITCODE}: $Exe $($Arguments -join ' ')"
    }
}

function Require-Application([string] $Name, [string] $Install) {
    $command = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $command) { throw "$Name is required. $Install" }
    $command.Source
}

function Assert-Msvc {
    $hint = 'Use Visual Studio Installer > Desktop development with C++ (MSVC x64/x86 tools and Windows SDK).'
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf) -or -not (
        Invoke-Native $vswhere @('-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
            '-find', 'VC\Tools\MSVC\**\bin\Hostx64\x64\link.exe')
    )) { throw "MSVC x64 linker not found. $hint" }

    $sdkRoot = Get-ItemPropertyValue 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots' `
        -Name KitsRoot10 -ErrorAction SilentlyContinue
    $sdk = if ($sdkRoot) {
        Get-ChildItem -LiteralPath (Join-Path $sdkRoot 'Lib') -Directory -ErrorAction SilentlyContinue | Where-Object {
            (Test-Path -LiteralPath (Join-Path $_.FullName 'um\x64\kernel32.lib') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $_.FullName 'ucrt\x64\ucrt.lib') -PathType Leaf)
        } | Select-Object -First 1
    }
    if (-not $sdk) { throw "Windows SDK with x64 libraries not found. $hint" }
}

function Ensure-Rust([bool] $Install) {
    $installed = Invoke-Native $rustup @('toolchain', 'list') | ForEach-Object { ($_ -split '\s+')[0] }
    if ($toolchain -notin $installed) {
        if (-not $Install) { throw "Rust $toolchain is missing. Run scripts/build-device.ps1 -Setup." }
        Invoke-Native $rustup @('toolchain', 'install', $toolchain, '--profile', 'minimal', '--no-self-update') | Out-Host
    }
    $targets = Invoke-Native $rustup @('target', 'list', '--installed', '--toolchain', $toolchain)
    if ($target -notin $targets) {
        if (-not $Install) { throw "Rust target $target is missing. Run scripts/build-device.ps1 -Setup." }
        Invoke-Native $rustup @('target', 'add', '--toolchain', $toolchain, $target) | Out-Host
    }
    if ($Install) { Write-Host "Ready: Rust $toolchain with $target" }
}

function New-Stage {
    $null = [IO.Directory]::CreateDirectory($toolRoot)
    $stage = Join-Path $toolRoot ('.stage-' + [guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($stage).FullName
}

function Publish-Directory([string] $Source, [string] $Destination) {
    try {
        # Unlike Move-Item, this never nests the source in an existing destination.
        [IO.Directory]::Move($Source, $Destination)
    } catch {
        if (Test-Path -LiteralPath $Destination) { throw "Cache path '$Destination' already exists. Rerun to use it." }
        throw
    }
}

function Assert-Tool([hashtable] $Tool, [string] $Directory) {
    $exe = Join-Path $Directory "$($Tool.Name).exe"
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf) -or
        ($Tool.Name -eq 'zig' -and -not (Test-Path -LiteralPath (Join-Path $Directory 'lib') -PathType Container))) {
        throw "Missing $($Tool.Name) executable or distribution files in '$Directory'."
    }
    $actual = (Invoke-Native $exe $Tool.VersionArgs) -join "`n"
    $expected = $Tool.VersionPrefix + $Tool.Version
    if ($actual -cne $expected) { throw "Expected '$expected' at '$exe', got '$actual'." }
}

function Get-PinnedTool([hashtable] $Tool, [bool] $Install) {
    $destination = Join-Path $toolRoot "$($Tool.Name)-$($Tool.Version)"
    $exe = Join-Path $destination "$($Tool.Name).exe"
    if (Test-Path -LiteralPath $destination) {
        try { Assert-Tool $Tool $destination }
        catch { throw "Invalid tool cache '$destination'. Move it aside and rerun -Setup. $_" }
        if ($Install) { Write-Host "Already installed: $exe" }
        return $exe
    }
    if (-not $Install) { throw "Missing '$exe'. Run scripts/build-device.ps1 -Setup." }

    $stage = New-Stage
    try {
        $zip = Join-Path $stage 'download.zip'
        $expanded = Join-Path $stage 'expanded'
        Write-Host "Downloading $($Tool.Name) $($Tool.Version)"
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $Tool.Url -OutFile $zip
        $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash
        if ($actual -ine $Tool.Sha256) {
            throw "SHA-256 mismatch for $($Tool.Url): expected $($Tool.Sha256), got $actual."
        }
        Expand-Archive -LiteralPath $zip -DestinationPath $expanded
        $source = $expanded
        if ($Tool.Name -eq 'zig') {
            $executables = @(Get-ChildItem -LiteralPath $expanded -Recurse -File -Filter 'zig.exe')
            if ($executables.Count -ne 1) { throw 'Expected exactly one zig.exe in the Zig ZIP.' }
            $source = $executables[0].DirectoryName
        }
        Assert-Tool $Tool $source
        Publish-Directory $source $destination
        Write-Host "Installed: $exe"
    } finally {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
    $exe
}

function Assert-Revision([string] $Directory, [string] $Expected) {
    $actual = Invoke-Native $git @('-C', $Directory, 'rev-parse', '--verify', 'HEAD')
    if ($actual -cne $Expected) { throw "Expected commit $Expected at '$Directory', got $actual." }
}

function Get-TouchSources {
    # Keep the source tags and the patch in sync with touch-ui/rs/setup-src.sh.
    $lvglRev = '85aa60d18b3d5e5588d7b247abf90198f07c8a63'
    $ftRev = '42608f77f20749dd6ddc9e0536788eaad70ea4b5'
    $clone = @('-c', 'core.autocrlf=false', '-c', 'core.eol=lf', '-c', 'core.longpaths=true',
        '-c', 'advice.detachedHead=false', 'clone', '--depth', '1', '--branch')
    $stage = New-Stage
    try {
        $patch = Join-Path $stage 'lvgl.patch'
        $original = Join-Path $root 'touch-ui\patches\lvgl-v9.5.0-freetype-bold.patch'
        $utf8 = [Text.UTF8Encoding]::new($false, $true)
        $text = [IO.File]::ReadAllText($original, $utf8).Replace("`r`n", "`n").Replace("`r", "`n")
        [IO.File]::WriteAllText($patch, $text, $utf8)
        $sha = (Get-FileHash -LiteralPath $patch -Algorithm SHA256).Hash.ToLowerInvariant()
        $destination = Join-Path $toolRoot "src-$($lvglRev.Substring(0, 12))-$($ftRev.Substring(0, 12))-$($sha.Substring(0, 16))"

        if (Test-Path -LiteralPath $destination) {
            $lvgl = Join-Path $destination 'lvgl'
            $ft = Join-Path $destination 'freetype'
            try {
                foreach ($directory in @((Join-Path $lvgl 'src'), (Join-Path $ft 'include'), (Join-Path $ft 'src'))) {
                    if (-not (Test-Path -LiteralPath $directory -PathType Container)) { throw "Missing '$directory'." }
                }
                Assert-Revision $lvgl $lvglRev
                Assert-Revision $ft $ftRev
                Invoke-Native $git @('-C', $lvgl, '-c', 'core.autocrlf=false', 'apply', '-R', '--check', '-p3', $patch)
            } catch {
                throw "Invalid touch source cache '$destination'. Move it aside and rerun the build. $_"
            }
            Write-Host "Using touch sources: $destination"
        } else {
            $sources = Join-Path $stage 'sources'
            $null = [IO.Directory]::CreateDirectory($sources)
            $lvgl = Join-Path $sources 'lvgl'
            $ft = Join-Path $sources 'freetype'
            Invoke-Native $git ($clone + @('v9.5.0', 'https://github.com/lvgl/lvgl.git', $lvgl)) | Out-Host
            Assert-Revision $lvgl $lvglRev
            $apply = @('-C', $lvgl, '-c', 'core.autocrlf=false', 'apply', '-p3')
            Invoke-Native $git ($apply + @('--check', $patch))
            Invoke-Native $git ($apply + @($patch))
            Invoke-Native $git ($clone + @('VER-2-13-3', 'https://gitlab.freedesktop.org/freetype/freetype.git', $ft)) | Out-Host
            Assert-Revision $ft $ftRev
            Publish-Directory $sources $destination
            Write-Host "Prepared touch sources: $destination"
        }
        $destination
    } finally {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
}

function Assert-CleanBuildEnvironment {
    $names = @('CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER', 'CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS',
        'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_RUSTFLAGS')
    foreach ($prefix in @('CC', 'CXX', 'AR', 'RANLIB')) {
        $names += "${prefix}_$target", "${prefix}_$($target.Replace('-', '_'))"
    }
    $conflicts = $names | Where-Object { [Environment]::GetEnvironmentVariable($_) }
    if ($conflicts) { throw "Conflicting build environment: $($conflicts -join ', '). Rerun from a clean PowerShell." }
}

function Build-Device([string] $ZigExe, [string] $ZigbuildExe) {
    $commit = Invoke-Native $git @('rev-parse', '--verify', 'HEAD^{commit}')
    if (Invoke-Native $git @('--no-optional-locks', 'status', '--porcelain', '--untracked-files=no')) {
        Write-Warning "Building modified tracked files; the version stamp records base HEAD $commit."
    }
    $cargo = Invoke-Native $rustup @('which', '--toolchain', $toolchain, 'cargo')
    $rustc = Invoke-Native $rustup @('which', '--toolchain', $toolchain, 'rustc')
    $targetDir = Join-Path $root 'target'
    $packages = [ordered]@{
        core     = 'luyaz-core'
        web      = 'luyaz-web'
        sentinel = 'luyaz-sentinel'
        touch    = 'luyaz-ui'
    }

    foreach ($name in $packages.Keys) {
        if ($Component -ne 'all' -and $Component -ne $name) { continue }
        $package = $packages[$name]
        $profile = if ($name -eq 'web') { 'release' } else { 'release-device' }
        $start = [Diagnostics.ProcessStartInfo]::new($rustup)
        $start.WorkingDirectory = $root
        $start.UseShellExecute = $false
        foreach ($argument in @('run', $toolchain, $ZigbuildExe, 'zigbuild', '--locked', '--profile', $profile,
            '--target', $target, '-p', $package, '--target-dir', $targetDir)) {
            $start.ArgumentList.Add($argument)
        }
        $environment = $start.Environment
        $environment['CARGO'] = $cargo
        $environment['RUSTC'] = $rustc
        $environment['LUYAZ_COMMIT'] = $commit
        $environment['CARGO_ZIGBUILD_ZIG_PATH'] = $ZigExe
        $environment['CARGO_ZIGBUILD_ZIG_COMMAND'] = $ZigExe
        $environment['CARGO_ZIGBUILD_CACHE_DIR'] = Join-Path $toolRoot 'zigbuild-cache'
        $environment['ZIG_GLOBAL_CACHE_DIR'] = Join-Path $toolRoot 'zig-cache'
        foreach ($variable in @('CARGO_ZIGBUILD_ZIG_COMMAND_ARGS', 'CARGO_ZIGBUILD_ZIG_VERSION',
            'CARGO_ZIGBUILD_RUSTC_VERSION', 'LVGL_DIR', 'FT_SRC', 'LV_CONF_DIR')) {
            $null = $environment.Remove($variable)
        }
        if ($name -eq 'touch') {
            $sources = Get-TouchSources
            $environment['LVGL_DIR'] = Join-Path $sources 'lvgl'
            $environment['FT_SRC'] = Join-Path $sources 'freetype'
            $environment['LV_CONF_DIR'] = Join-Path $root 'touch-ui'
        }

        Write-Host "Building $package ($profile, $target, $commit)"
        $process = [Diagnostics.Process]::Start($start)
        try {
            $process.WaitForExit()
            if ($process.ExitCode -ne 0) { throw "Building $package failed (exit $($process.ExitCode))." }
        } finally {
            $process.Dispose()
        }
        $artifact = Join-Path $targetDir $target $profile $package
        if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) { throw "Missing build artifact '$artifact'." }
        $artifact
    }
}

try {
    if (-not $IsWindows -or [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture -ne 'X64' -or
        [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
        throw 'This script requires PowerShell 7 for x64 on x64 Windows.'
    }
    $root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
    $toolRoot = Join-Path $env:LOCALAPPDATA 'LuyaZ\build'
    Push-Location -LiteralPath $root
    try {
        $git = Require-Application 'git.exe' 'Install Git for Windows and put git.exe on PATH.'
        $rustup = Require-Application 'rustup.exe' 'Install Rust via rustup for x64 Windows (MSVC).'
        Assert-Msvc
        $install = $PSCmdlet.ParameterSetName -eq 'Setup'
        if (-not $install) { Assert-CleanBuildEnvironment }
        Ensure-Rust $install
        $zigExe = Get-PinnedTool $zig $install
        $zigbuildExe = Get-PinnedTool $zigbuild $install
        if (-not $install) { Build-Device $zigExe $zigbuildExe }
    } finally {
        Pop-Location
    }
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
