<#
Builds the trimmed JRE bundled for the LanguageTool subprocess, into
src-tauri/resources/jre/. Gitignored, since it is a generated binary tens of
megabytes in size: this script is what reproduces it, not a copy committed to
the repository.

The module list below was derived by running, from a LanguageTool 6.6
distribution's own directory:

    jdeps --print-module-deps --multi-release 21 --class-path "libs/*" `
        --ignore-missing-deps languagetool-server.jar

The ignored dependencies are optional metrics integrations (resilience4j,
OpenTelemetry) that languagetool-server.jar references but never loads for
plain grammar checking over HTTP. The resulting module list was then verified
by building the JRE below with it and running a real LanguageTool 6.6 server
under it, checking real English (GB) text successfully.

Usage: build-languagetool-jre.ps1 -LanguageToolDir <path to an extracted
LanguageTool distribution>
#>

param(
    [Parameter(Mandatory = $true)]
    [string]$LanguageToolDir
)

$ErrorActionPreference = "Stop"

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

Write-Host "languagetool-server.jar, libs/, org/ and META-INF/ copied to $languagetoolResourcesDir"
