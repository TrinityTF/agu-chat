package lol.agu.chat.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** The desktop's sections that make sense on a phone (Hotkeys is left out), in the desktop's order. */
private val sections = listOf(
    "accounts" to "Accounts",
    "appearance" to "Appearance and chat",
    "highlights" to "Highlights",
    "ignores" to "Ignores",
    "modbuttons" to "Mod buttons",
    "updates" to "Updates",
    "report" to "Report a bug",
    "about" to "About",
)

/**
 * Settings: a list of the sections, each its own page. [start] is the section to open on ("" for the list), and
 * where Back goes: a section opened from the list returns to it, one opened directly (a login prompt) closes Settings.
 */
@Composable
fun SettingsScreen(vm: ChatViewModel, start: String, onClose: () -> Unit) {
    var section by rememberSaveable { mutableStateOf(start) }
    val back: () -> Unit = { if (start.isEmpty()) section = "" else onClose() }
    when (section) {
        "accounts" -> AccountsScreen(vm, onBack = back)
        "appearance" -> AppearanceSettings(vm, onBack = back)
        "highlights" -> HighlightsSettings(vm, onBack = back)
        "ignores" -> IgnoresSettings(vm, onBack = back)
        "modbuttons" -> ModButtonsSettings(vm, onBack = back)
        "updates" -> UpdatesSettings(vm, onBack = back)
        "report" -> ReportSettings(vm, onBack = back)
        "about" -> AboutSettings(vm, onBack = back)
        else -> {
            BackHandler(onBack = onClose)
            SettingsPage("Settings", onClose) {
                SettingsCard {
                    sections.forEachIndexed { i, (id, title) ->
                        Row(
                            Modifier.fillMaxWidth().clickable { section = id }.padding(horizontal = 16.dp, vertical = 15.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(title, color = AguColors.Text, fontSize = 16.sp, modifier = Modifier.weight(1f))
                            if (id == "updates" && vm.updater.status in listOf("available", "ready")) {
                                Text("●", color = AguColors.Accent, fontSize = 12.sp, modifier = Modifier.padding(end = 10.dp))
                            }
                            Text("›", color = AguColors.Muted, fontSize = 20.sp)
                        }
                        if (i < sections.lastIndex) Divider()
                    }
                }
            }
        }
    }
}

// ---- Appearance and chat ---------------------------------------------------------------------------------------------

/** The desktop's labels, hints and order. Left out as desktop-only: Window (always on top, start with Windows) and Pause chat (mouse or Alt). Background is the phone's own. */
@Composable
private fun AppearanceSettings(vm: ChatViewModel, onBack: () -> Unit) {
    val s = vm.settings
    SettingsPage("Appearance and chat", onBack) {
        SettingsHeading("Messages")
        SettingsCard {
            SwitchRow("Show timestamps", null, s.bool("timestamps")) { vm.patchSettings(jo("timestamps" to it)) }
            SwitchRow("Timestamps with seconds", null, s.bool("timestampSeconds"), enabled = s.bool("timestamps")) { vm.patchSettings(jo("timestampSeconds" to it)) }
            SwitchRow("Alternate row backgrounds", null, s.bool("alternateRows", true)) { vm.patchSettings(jo("alternateRows" to it)) }
            SliderRow("Font size", "Chat text, in pixels", s.num("fontSize", 13f), 10f, 22f, 1f, "px") { vm.patchSettings(jo("fontSize" to it.toInt())) }
            SwitchRow("Readable name colors", "Lighten dark name colors so they stay legible", s.bool("readableColors", true)) { vm.patchSettings(jo("readableColors" to it)) }
            SwitchRow("Platform marks on rows", "A small platform icon in merged co-chats", s.bool("showPlatformMarks", true)) { vm.patchSettings(jo("showPlatformMarks" to it)) }
            SwitchRow("Link previews", "A card with the title and picture under Twitch clip, YouTube, X, Streamable and Instagram links", s.bool("linkPreviews", true)) { vm.patchSettings(jo("linkPreviews" to it)) }
            SwitchRow("Hype train bar", "Twitch's hype train at the top of the chat while one is going: level, time left and progress", s.bool("hypeTrain", true)) { vm.patchSettings(jo("hypeTrain" to it)) }
        }
        SettingsHeading("Background")
        SettingsCard {
            SwitchRow("Stay connected in the background", "Keeps your chats connected with a small notification, so mentions and highlights still alert you. Off lets the phone pause the app", vm.stayConnected) { vm.changeStayConnected(it) }
        }
        SettingsHeading("Emotes and badges")
        SettingsCard {
            ChoiceRow("Emote size", null, s.str("emoteSize", "medium"), listOf("small" to "Small", "medium" to "Medium", "large" to "Large")) { vm.patchSettings(jo("emoteSize" to it)) }
            SwitchRow("Show badges", null, s.bool("showBadges", true)) { vm.patchSettings(jo("showBadges" to it)) }
            SwitchRow("7TV emotes", null, s.bool("sevenTv", true)) { vm.patchSettings(jo("sevenTv" to it)) }
            SwitchRow("BetterTTV emotes", null, s.bool("bttv", true)) { vm.patchSettings(jo("bttv" to it)) }
            SwitchRow("FrankerFaceZ emotes", null, s.bool("ffz", true)) { vm.patchSettings(jo("ffz" to it)) }
            SwitchRow("7TV cosmetics", "Name paints and badges from 7TV", s.bool("sevenTvCosmetics", true)) { vm.patchSettings(jo("sevenTvCosmetics" to it)) }
            SwitchRow("Animated emotes", "Off keeps emotes still and uses far less battery. Changing it reloads the chats", s.bool("animatedEmotes", true)) { vm.patchSettings(jo("animatedEmotes" to it)) }
        }
        SettingsHeading("Moderation")
        SettingsCard {
            ChoiceRow("Deleted messages", null, s.str("deletedMessages", "strike"), listOf("strike" to "Grey out", "hide" to "Hide", "show" to "Show normally")) { vm.patchSettings(jo("deletedMessages" to it)) }
            SwitchRow("Show mod buttons", "Inline buttons on rows when mod mode is on", s.bool("showModButtons", true)) { vm.patchSettings(jo("showModButtons" to it)) }
            SwitchRow("Show moderation actions", "Bans, timeouts and mode changes as lines in chat", s.bool("showModActions", true)) { vm.patchSettings(jo("showModActions" to it)) }
            SwitchRow("Show AutoMod", "Messages AutoMod held, with Allow and Deny", s.bool("showAutomod", true)) { vm.patchSettings(jo("showAutomod" to it)) }
        }
    }
}

// ---- Report a bug and About -------------------------------------------------------------------------------------------

internal fun openLink(context: Context, url: String) {
    runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) }
}

@Composable
private fun LinkText(text: String, url: String) {
    val ctx = LocalContext.current
    Text(text, color = AguColors.Accent, fontSize = 15.sp, fontWeight = FontWeight.Medium, modifier = Modifier.clickable { openLink(ctx, url) }.padding(vertical = 6.dp))
}

/** Like the desktop: GitHub's new-issue page with the version and system filled in. */
@Composable
private fun ReportSettings(vm: ChatViewModel, onBack: () -> Unit) {
    val m = vm.settingsMeta
    SettingsPage("Report a bug", onBack) {
        SettingsCard(Modifier.padding(16.dp)) {
            Text("Found a bug or have an idea? Open an issue on GitHub. Your app version and system are filled in for you.", color = AguColors.Text, fontSize = 15.sp)
            Text("Helpful to include:", color = AguColors.Muted, fontSize = 14.sp, modifier = Modifier.padding(top = 8.dp))
            for (t in listOf(
                "What you did, what you expected, and what happened instead.",
                "Which platform and chat it was on (Twitch, Kick, YouTube, Rumble or X).",
                "A screenshot or a short clip, if you can.",
            )) Text("• $t", color = AguColors.Muted, fontSize = 14.sp)
            Column(Modifier.padding(top = 12.dp)) {
                LinkText("Report a bug on GitHub", m.str("newIssueUrl", "https://github.com/TrinityTF/agu-chat/issues/new"))
                LinkText("See reported bugs", m.str("issuesUrl", "https://github.com/TrinityTF/agu-chat/issues"))
            }
        }
    }
}

@Composable
private fun AboutSettings(vm: ChatViewModel, onBack: () -> Unit) {
    val repo = vm.settingsMeta.str("repoUrl", "https://github.com/TrinityTF/agu-chat")
    SettingsPage("About", onBack) {
        SettingsCard(Modifier.padding(16.dp)) {
            Text("Agu Chat", color = AguColors.Text, fontSize = 20.sp, fontWeight = FontWeight.Bold)
            Text("Version ${vm.appVersion}", color = AguColors.Muted, fontSize = 15.sp)
            Text("Live chat for Twitch, Kick, YouTube, Rumble and X in one app.", color = AguColors.Muted, fontSize = 15.sp, modifier = Modifier.padding(top = 8.dp))
            Column(Modifier.padding(top = 10.dp)) { LinkText(repo, repo) }
        }
    }
}
