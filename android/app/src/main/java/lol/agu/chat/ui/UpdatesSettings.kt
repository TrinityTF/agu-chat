package lol.agu.chat.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.FileProvider
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import java.io.File

/** The updater as the core reports it (`updater` event): status, the desktop's status line, progress and the release notes. */
data class UpdaterInfo(
    /** idle | dev | checking | none | available | downloading | ready | error */
    val status: String = "idle",
    val channel: String = "beta",
    val version: String? = null,
    val notes: List<String> = emptyList(),
    val percent: Int = 0,
    val error: String? = null,
    val text: String = "",
) {
    companion object {
        fun parse(e: JsonElement): UpdaterInfo {
            val o = e as? JsonObject ?: return UpdaterInfo()
            return UpdaterInfo(
                status = o.str("status", "idle"),
                channel = o.str("channel", "beta"),
                version = (o["version"] as? JsonPrimitive)?.contentOrNull,
                notes = (o["notes"] as? JsonArray)?.mapNotNull { (it as? JsonPrimitive)?.contentOrNull } ?: emptyList(),
                percent = (o["percent"] as? JsonPrimitive)?.intOrNull ?: 0,
                error = (o["error"] as? JsonPrimitive)?.contentOrNull,
                text = o.str("text"),
            )
        }
    }
}

/** Hands a downloaded, verified APK to Android's installer. Nothing is installed without the person confirming in the system dialog. */
object ApkInstaller {
    /** Android lets an app install other apps only after the person allowed it for that app (Settings). */
    fun mayInstall(context: Context): Boolean = context.packageManager.canRequestPackageInstalls()

    /** The system page where installs from this app are allowed. */
    fun permissionIntent(context: Context): Intent =
        Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)

    fun install(context: Context, path: String): Boolean = runCatching {
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.updates", File(path))
        context.startActivity(
            Intent(Intent.ACTION_VIEW).setDataAndType(uri, "application/vnd.android.package-archive")
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK),
        )
        true
    }.getOrDefault(false)
}

/** Settings -> Updates: the channel, the check, and the download and install of a newer APK. */
@Composable
fun UpdatesSettings(vm: ChatViewModel, onBack: () -> Unit) {
    val ctx = LocalContext.current
    val u = vm.updater
    val busy = u.status == "checking" || u.status == "downloading"
    var askPermission by remember { mutableStateOf(false) }
    SettingsPage("Updates", onBack) {
        SettingsCard(Modifier.padding(14.dp)) {
            Text("UPDATE CHANNEL", color = AguColors.Muted, fontSize = 11.sp, modifier = Modifier.padding(bottom = 8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for (ch in listOf("beta", "stable")) {
                    val on = vm.settings.str("updateChannel", "beta") == ch
                    Column(
                        Modifier.weight(1f).clip(RoundedCornerShape(12.dp)).background(if (on) AguColors.Accent.copy(alpha = 0.16f) else Color.Transparent)
                            .border(1.dp, if (on) AguColors.Accent else AguColors.Surface2, RoundedCornerShape(12.dp))
                            .clickable { vm.patchSettings(jo("updateChannel" to ch)) }.padding(12.dp),
                    ) {
                        Text(if (ch == "beta") "Beta" else "Stable", color = AguColors.Text, fontWeight = FontWeight.SemiBold, fontSize = 15.sp)
                        Text(
                            if (ch == "beta") "Prereleases: the newest features, with rough edges." else "Full releases only: fewer updates, better tested.",
                            color = AguColors.Muted, fontSize = 12.sp, lineHeight = 16.sp,
                        )
                    }
                }
            }
        }
        SettingsCard(Modifier.padding(14.dp)) {
            Row {
                Text("Current version ", color = AguColors.Text, fontSize = 15.sp)
                Text(vm.appVersion, color = AguColors.Text, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
            }
            Text(u.text, color = AguColors.Muted, fontSize = 14.sp, modifier = Modifier.padding(top = 2.dp))
            Row(Modifier.padding(top = 10.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Chip("Check now", enabled = !busy) { vm.updaterCheck() }
                when (u.status) {
                    "available" -> AccentButton("Download ${u.version ?: ""}".trim()) { vm.updaterDownload() }
                    "ready" -> AccentButton("Install ${u.version ?: ""}".trim()) { vm.updaterInstall(ctx) { askPermission = true } }
                }
            }
            if (u.status == "downloading") {
                Box(Modifier.padding(top = 10.dp).fillMaxWidth().height(6.dp).clip(RoundedCornerShape(4.dp)).background(AguColors.Bg)) {
                    Box(Modifier.fillMaxWidth(u.percent.coerceIn(2, 100) / 100f).height(6.dp).background(AguColors.Accent))
                }
            }
            if (u.notes.isNotEmpty()) {
                Column(Modifier.padding(top = 10.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    for (n in u.notes) Row {
                        Text("•", color = AguColors.Muted, fontSize = 12.sp, modifier = Modifier.padding(end = 8.dp))
                        Text(n, color = AguColors.Muted, fontSize = 12.sp, lineHeight = 16.sp)
                    }
                }
            }
        }
        if (u.status == "ready") SettingsNote("The download was checked against the release's checksum. Android asks you to confirm before it installs.")
    }
    if (askPermission) AlertDialog(
        onDismissRequest = { askPermission = false },
        containerColor = AguColors.Surface,
        title = { Text("Allow installing updates", color = AguColors.Text) },
        text = {
            Text(
                "Android only lets an app install an update when you allow it. The next screen is Android's own setting for Agu Chat: switch on \"Allow from this source\", then come back and tap Install.",
                color = AguColors.Muted,
            )
        },
        confirmButton = {
            TextButton(onClick = { askPermission = false; runCatching { ctx.startActivity(ApkInstaller.permissionIntent(ctx)) } }) { Text("Open the setting", color = AguColors.Accent) }
        },
        dismissButton = { TextButton(onClick = { askPermission = false }) { Text("Not now", color = AguColors.Muted) } },
    )
}
