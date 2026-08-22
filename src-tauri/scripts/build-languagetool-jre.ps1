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

if (Test-Path $outputDir) {
    Remove-Item -Recurse -Force $outputDir
}

jlink --add-modules $modules `
    --output $outputDir `
    --strip-debug `
    --no-man-pages `
    --no-header-files `
    --compress=2

Write-Host "Trimmed JRE built at $outputDir"

$languagetoolResourcesDir = Join-Path $repoRoot "resources\languagetool"
New-Item -ItemType Directory -Force -Path $languagetoolResourcesDir | Out-Null
Copy-Item (Join-Path $LanguageToolDir "languagetool-server.jar") $languagetoolResourcesDir -Force
Copy-Item (Join-Path $LanguageToolDir "libs") (Join-Path $languagetoolResourcesDir "libs") -Recurse -Force

Write-Host "languagetool-server.jar and libs/ copied to $languagetoolResourcesDir"
