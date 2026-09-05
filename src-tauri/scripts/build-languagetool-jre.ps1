<#
Builds the two resources the LanguageTool subprocess needs: the trimmed JRE it
runs under, into src-tauri/resources/jre/, and the LanguageTool distribution
itself, into src-tauri/resources/languagetool/. Both directories are tracked but
their contents are gitignored, being generated and fetched binaries of a few
hundred megabytes: this script is what reproduces them, not a copy committed to
the repository. A fresh checkout has them empty, and the app degrades to
spelling and AI-tell flags until this runs.

The module list below was derived by running, from a LanguageTool 6.6
distribution's own directory:

    jdeps --print-module-deps --multi-release 21 --class-path "libs/*" `
        --ignore-missing-deps languagetool-server.jar

The ignored dependencies are optional metrics integrations (resilience4j,
OpenTelemetry) that languagetool-server.jar references but never loads for
plain grammar checking over HTTP. The resulting module list was then verified
by building the JRE below with it and running a real LanguageTool 6.6 server
under it, checking real English (GB) text successfully.

Requires a JDK on PATH for jlink.

Usage: build-languagetool-jre.ps1 [-LanguageToolDir <path to an extracted
LanguageTool distribution>]

Without -LanguageToolDir the pinned release below is downloaded and checked
against its SHA-256, so a fresh checkout needs nothing already on disk.
#>

param(
    [Parameter(Mandatory = $false)]
    [string]$LanguageToolDir
)

$ErrorActionPreference = "Stop"

# Pinned rather than tracking whatever "latest" resolves to: the jlink module list below was
# derived against this exact release, and the server it produces is bundled into a shipped build.
# languagetool.org publishes no checksum file alongside the download, so this hash was taken from
# the archive this project's own resources were built from, then confirmed byte-identical to a
# fresh download of the URL below.
$languageToolVersion = "6.6"
$languageToolUrl = "https://languagetool.org/download/LanguageTool-$languageToolVersion.zip"
$languageToolSha256 = "53600506B399BB5FFE1E4C8DEC794FD378212F14AAF38CCEF9B6F89314D11631"

$modules = "java.base,java.compiler,java.desktop,java.instrument,java.naming," +
           "java.scripting,java.sql,jdk.attach,jdk.httpserver,jdk.jdi," +
           "jdk.management,jdk.unsupported"

$repoRoot = Split-Path -Parent $PSScriptRoot
$outputDir = Join-Path $repoRoot "resources\jre"
$stagingDir = Join-Path ([System.IO.Path]::GetTempPath()) ("wa-jre-" + [guid]::NewGuid().ToString("N"))

# jlink refuses to write into a directory that already exists, while resources/jre/ has to keep
# existing in a fresh checkout, since tauri.conf.json lists it as a bundled resource and a
# resource path missing at build time fails the build outright. Staging outside the repository
# and moving the contents in satisfies both, and leaves .gitkeep in place.
jlink --add-modules $modules `
    --output $stagingDir `
    --strip-debug `
    --no-man-pages `
    --no-header-files `
    --compress=2

# $ErrorActionPreference does not apply to a native executable's exit code, so without this an
# unusable jlink would fall through to the wipe below and destroy a working resources\jre.
if ($LASTEXITCODE -ne 0) {
    throw "jlink failed with exit code $LASTEXITCODE; resources\jre left untouched"
}

New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
Get-ChildItem -Force $outputDir | Where-Object { $_.Name -ne ".gitkeep" } | Remove-Item -Recurse -Force
Move-Item (Join-Path $stagingDir "*") $outputDir -Force
Remove-Item -Recurse -Force $stagingDir

Write-Host "Trimmed JRE built at $outputDir"

# Resolved after jlink, not before: a missing JDK is the cheap failure and should surface before
# a few hundred megabytes are pulled over the network.
$downloadDir = $null
if (-not $LanguageToolDir) {
    $downloadDir = Join-Path ([System.IO.Path]::GetTempPath()) ("wa-lt-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $downloadDir | Out-Null
    $zipPath = Join-Path $downloadDir "LanguageTool-$languageToolVersion.zip"

    Write-Host "Downloading $languageToolUrl"
    # Invoke-WebRequest renders its progress bar per chunk in Windows PowerShell 5.1, which costs
    # more than the transfer itself on a download this size.
    $previousProgress = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest -Uri $languageToolUrl -OutFile $zipPath -UseBasicParsing
    } finally {
        $ProgressPreference = $previousProgress
    }

    $actualSha256 = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash
    if ($actualSha256 -ne $languageToolSha256) {
        throw "LanguageTool download failed its checksum: expected $languageToolSha256, got $actualSha256"
    }

    # ExtractToDirectory rather than Expand-Archive: Expand-Archive pipes every entry through the
    # object pipeline, which on this archive's tens of thousands of rule and dictionary files
    # takes many times longer.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($zipPath, $downloadDir)
    $LanguageToolDir = Join-Path $downloadDir "LanguageTool-$languageToolVersion"
    Write-Host "Verified and extracted to $LanguageToolDir"
}

if (-not (Test-Path (Join-Path $LanguageToolDir "languagetool-server.jar"))) {
    throw "no languagetool-server.jar under $LanguageToolDir; pass an extracted LanguageTool distribution, or omit -LanguageToolDir to fetch the pinned one"
}

$languagetoolResourcesDir = Join-Path $repoRoot "resources\languagetool"
New-Item -ItemType Directory -Force -Path $languagetoolResourcesDir | Out-Null

# org/ and META-INF/ are copied alongside the jar and libs/ because languagetool-server.jar's own
# manifest Class-Path begins `./`: the JVM resolves the language modules from the jar's directory,
# and they live in org/ as loose class and data files, with META-INF/ carrying the
# language-module.properties that enumerates them. Neither is inside any jar under libs/. With
# libs/ alone beside the jar, the server starts and then dies in LanguageIdentifier's static
# initialiser knowing only xx-XX, LanguageTool's own test fixture language.
foreach ($stale in @("languagetool-server.jar", "libs", "org", "META-INF")) {
    $staleTarget = Join-Path $languagetoolResourcesDir $stale
    if (Test-Path $staleTarget) { Remove-Item -Recurse -Force $staleTarget }
}

Copy-Item (Join-Path $LanguageToolDir "languagetool-server.jar") $languagetoolResourcesDir -Force
foreach ($dir in @("libs", "org", "META-INF")) {
    Copy-Item (Join-Path $LanguageToolDir $dir) (Join-Path $languagetoolResourcesDir $dir) -Recurse -Force
}

if ($downloadDir) {
    Remove-Item -Recurse -Force $downloadDir
}

Write-Host "languagetool-server.jar, libs/, org/ and META-INF/ copied to $languagetoolResourcesDir"
