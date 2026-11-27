package lol.agu.chat.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import lol.agu.chat.Core

/** What an ignored user is stored as: login without "@", lowercase (the core cleans it the same way). */
private fun ignoreLogin(text: String) = text.trim().trimStart('@').lowercase()

private val ACTIONS = listOf("hide" to "Hide message", "replace" to "Replace with")

/** Settings -> Ignores: users (every split hides them) and phrases (hide the message, or replace the matching text). */
@Composable
fun IgnoresSettings(vm: ChatViewModel, onBack: () -> Unit) {
    val s = vm.settings
    val users = s.list("ignoredUsers")
    val phrases = s.list("ignoredPhrases")
    var platform by remember { mutableStateOf("twitch") }
    var typed by remember { mutableStateOf("") }
    val login = ignoreLogin(typed)

    fun addUser() {
        if (login.isEmpty()) return
        if (users.none { it.str("platform") == platform && it.str("login") == login }) {
            vm.patchSettings(jo("ignoredUsers" to (users + jo("platform" to platform, "login" to login))))
        }
        typed = ""
    }

    SettingsPage("Ignores", onBack) {
        SettingsHeading("Ignored users")
        SettingsNote("Messages from these users are hidden in every split. You can also use \"Ignore user\" on a user card.")
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            ChoiceBox(platform, platforms.map { it to platformLabel(it) }, { platform = it })
            DraftField(typed, { typed = it }, Modifier.weight(1f), placeholder = if (platform == "youtube") "Channel name" else "Username", onDone = ::addUser)
            Chip("Add", on = true, enabled = login.isNotEmpty(), onClick = ::addUser)
        }
        SettingsCard(Modifier.padding(horizontal = 16.dp, vertical = 6.dp)) {
            if (users.isEmpty()) Text("No ignored users.", color = AguColors.Muted, fontSize = 14.sp, modifier = Modifier.padding(vertical = 8.dp))
            users.sortedWith(compareBy({ platforms.indexOf(it.str("platform")) }, { it.str("login") })).forEach { u ->
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    Text(platformLabel(u.str("platform")), color = platformColor(u.str("platform")), fontSize = 13.sp, modifier = Modifier.padding(end = 10.dp))
                    Text(u.str("login"), color = AguColors.Text, fontSize = 15.sp, modifier = Modifier.weight(1f))
                    GlyphButton("✕", danger = true) { vm.patchSettings(jo("ignoredUsers" to users.filter { it != u })) }
                }
            }
        }

        SettingsHeading("Ignored phrases")
        SettingsNote("A message containing the phrase is hidden, or the matching text is replaced. .* treats the phrase as a regular expression, Aa matches case.")
        PhraseEditor(null, jo("pattern" to "", "regex" to false, "caseSensitive" to false, "action" to "hide", "replacement" to ""), onSave = { p ->
            vm.patchSettings(jo("ignoredPhrases" to (phrases + p)))
        }, onDelete = {})
        SettingsCard(Modifier.padding(12.dp)) {
            if (phrases.isEmpty()) Text("No ignored phrases.", color = AguColors.Muted, fontSize = 14.sp)
            phrases.forEachIndexed { i, p ->
                if (i > 0) Column(Modifier.padding(vertical = 6.dp)) { Divider() }
                PhraseEditor(
                    i, p,
                    onSave = { next -> vm.patchSettings(jo("ignoredPhrases" to phrases.mapIndexed { j, o -> if (j == i) next else o })) },
                    onDelete = { vm.patchSettings(jo("ignoredPhrases" to phrases.filterIndexed { j, _ -> j != i })) },
                )
            }
        }
    }
}

/** One phrase: the new one (index null, with an Add button) or a saved one (edits save as you go, with a bin). */
@Composable
private fun PhraseEditor(index: Int?, value: JsonObject, onSave: (JsonObject) -> Unit, onDelete: () -> Unit) {
    val adding = index == null
    var draft by remember(index, value) { mutableStateOf(value) }
    // the saved phrase changed from outside (another one was removed above it): show that one
    val v = if (adding) draft else value
    var pattern by remember(index, value.str("pattern")) { mutableStateOf(value.str("pattern")) }
    var error by remember { mutableStateOf<String?>(null) }
    val regex = v.bool("regex")
    val cs = v.bool("caseSensitive")
    LaunchedEffect(pattern, regex, cs) {
        error = if (pattern.isBlank()) null else runCatching {
            (Core.call("settings.check", jo("what" to "phrase", "pattern" to pattern, "regex" to regex, "caseSensitive" to cs)) as? JsonObject)?.get("error")?.let { (it as? JsonPrimitive)?.contentOrNull }
        }.getOrNull()
    }
    fun change(next: JsonObject) {
        if (adding) draft = next else onSave(next)
    }
    var gen by remember { mutableIntStateOf(0) }
    key(gen) { Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            CommitField(
                if (adding) draft.str("pattern") else value.str("pattern"), { change(v.with("pattern" to it)) }, Modifier.weight(1f),
                placeholder = if (regex) "Regular expression" else "Phrase", mono = regex, invalid = error != null,
                commitOnPause = !adding, onDraft = { pattern = it },
            )
            Chip(".*", on = regex) { change(v.with("regex" to !regex)) }
            Chip("Aa", on = cs) { change(v.with("caseSensitive" to !cs)) }
        }
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            ChoiceBox(v.str("action", "hide"), ACTIONS, { change(v.with("action" to it)) })
            if (v.str("action") == "replace") {
                CommitField(
                    if (adding) draft.str("replacement") else value.str("replacement"), { change(v.with("replacement" to it)) }, Modifier.weight(1f),
                    placeholder = "(nothing)", commitOnPause = !adding,
                )
            } else Row(Modifier.weight(1f)) {}
            if (adding) {
                val ok = pattern.isNotBlank() && error == null
                Chip("Add", on = true, enabled = ok) {
                    onSave(draft.with("pattern" to pattern))
                    draft = jo("pattern" to "", "regex" to false, "caseSensitive" to false, "action" to "hide", "replacement" to "")
                    pattern = ""
                    gen++
                }
            } else GlyphButton("✕", danger = true, onClick = onDelete)
        }
        error?.let { Text("Invalid regular expression: $it", color = AguColors.Danger, fontSize = 12.sp) }
    }
    }
}
