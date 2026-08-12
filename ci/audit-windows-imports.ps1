[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,
    [Parameter(Mandatory = $true)]
    [string]$Output
)

$ErrorActionPreference = "Stop"
$command = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
if ($null -ne $command) {
    $dumpbin = $command.Source
} else {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
        throw "could not locate dumpbin.exe or vswhere.exe"
    }
    $installation = & $vswhere -latest -products * -property installationPath
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($installation)) {
        throw "could not locate the Visual Studio installation"
    }
    $tools = Join-Path $installation "VC\Tools\MSVC"
    $candidates = @(Get-ChildItem -LiteralPath $tools -Filter dumpbin.exe -File -Recurse)
    $hostPath = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") {
        "*\Hostarm64\arm64\dumpbin.exe"
    } else {
        "*\Hostx64\x64\dumpbin.exe"
    }
    $command = $candidates | Where-Object FullName -Like $hostPath | Select-Object -Last 1
    if ($null -eq $command) {
        $command = $candidates | Sort-Object FullName | Select-Object -Last 1
    }
    $dumpbin = $command.FullName
}
if ($null -eq $command) {
    throw "could not locate dumpbin.exe"
}

$imports = @(& $dumpbin /nologo /dependents $Executable 2>&1)
if ($LASTEXITCODE -ne 0) {
    throw "dumpbin.exe failed for $Executable"
}
$imports | Set-Content -Encoding utf8NoBOM $Output
$forbidden = @($imports | Select-String -Pattern '(?i)\b(?:VCRUNTIME|MSVCP)[^\s]*\.dll\b')
if ($forbidden.Count -ne 0) {
    throw "Windows release binary imports the external VC++ runtime: $($forbidden -join ', ')"
}
