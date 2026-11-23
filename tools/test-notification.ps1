# Sends one Windows toast ("Islet test") so you can check that Islet shows it in the pill.
# Run it while Islet is running and no fullscreen app is in front: Windows does not pop
# toasts over fullscreen apps (and Islet is asleep then anyway).
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
$xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$texts = $xml.GetElementsByTagName('text')
$texts.Item(0).AppendChild($xml.CreateTextNode('Islet test')) | Out-Null
$texts.Item(1).AppendChild($xml.CreateTextNode('This should appear in the pill')) | Out-Null
$appId = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($appId).Show([Windows.UI.Notifications.ToastNotification]::new($xml))
Start-Sleep -Seconds 6
# Remove the test entry from the notification centre again.
[Windows.UI.Notifications.ToastNotificationManager]::History.Clear($appId)
