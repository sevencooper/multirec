# Generates the four speech clips used by the transcription end-to-end test
# (src/transcribe/mod.rs, test `transcribes_synthetic_session`) with Windows' built-in TTS.
#
#   powershell -ExecutionPolicy Bypass -File scripts\gen-test-speech.ps1 -OutDir C:\temp\multirec-tts
param([Parameter(Mandatory = $true)][string]$OutDir)
Add-Type -AssemblyName System.Speech
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(48000,
    [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
$lines = [ordered]@{
  "me1"    = "Hey, thanks for joining. Can you hear me okay?";
  "other1" = "Yes, I can hear you perfectly. What did you want to talk about today?";
  "me2"    = "I wanted to go over the recording tool. It is really important that the audio stays in sync.";
  "other2" = "That makes sense. Did the drift correction actually work?"
}
$voices = (New-Object System.Speech.Synthesis.SpeechSynthesizer).GetInstalledVoices() | ForEach-Object { $_.VoiceInfo.Name }
foreach ($k in $lines.Keys) {
  $s = New-Object System.Speech.Synthesis.SpeechSynthesizer
  # Use a second voice for the "other" speaker when one is installed.
  if ($k -like "other*" -and $voices.Count -gt 1) { $s.SelectVoice($voices[1]) } else { $s.SelectVoice($voices[0]) }
  $s.SetOutputToWaveFile((Join-Path $OutDir "$k.wav"), $fmt)
  $s.Speak($lines[$k])
  $s.Dispose()
}
Write-Host "Wrote clips to $OutDir"
