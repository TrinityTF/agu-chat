package lol.agu.chat.ui

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import lol.agu.chat.Core
import java.io.File
import java.util.UUID

private val KINDS = listOf("phrase" to "Phrase", "regex" to "Regex", "user" to "User", "badge" to "Badge")

private fun placeholderOf(kind: String) = when (kind) {
    "regex" -> "Regular expression, e.g. ^!(drop|claim)"
    "user" -> "Names, separated by commas"
    else -> "Word or phrase, e.g. giveaway"
}

// ---- picking a sound file --------------------------------------------------------------------------------------------

/**
 * The phone's file picker for an alert sound: the chosen file is copied out of the picker (it only gives a content
 * address) and the core stores it in its sounds folder, so it keeps playing if the original moves. [onPicked] gets the
 * stored name; [onError] the core's words (too big, not a sound file).
 */
@Composable
fun rememberSoundPicker(onPicked: (String) -> Unit, onError: (String) -> Unit): () -> Unit {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val picked by rememberUpdatedState(onPicked)
    val failed by rememberUpdatedState(onError)
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val copied = withContext(Dispatchers.IO) { copyPicked(ctx, uri) }
            val file = copied.getOrElse { failed(it.message ?: "Could not read that file."); return@launch }
            try {
                val r = Core.call("sounds.import", jo("path" to file.absolutePath)) as? JsonObject
                r?.str("file")?.takeIf { it.isNotEmpty() }?.let { picked(it) }
            } catch (e: Exception) {
                failed(e.message ?: "Could not use that file.")
            } finally {
                file.delete()
            }
        }
    }
    return { launcher.launch(arrayOf("audio/*")) }
}

private const val MAX_SOUND_BYTES = 5L * 1024 * 1024

private fun copyPicked(ctx: Context, uri: Uri): Result<File> = runCatching {
    val name = ctx.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
        if (c.moveToFirst()) c.getString(0) else null
    } ?: "sound"
    var clean = name.replace(Regex("[^A-Za-z0-9._ -]"), "_")
    if (!clean.contains('.') || clean.substringAfterLast('.').length > 5) {
        val ext = ctx.contentResolver.getType(uri)?.let { MimeTypeMap.getSingleton().getExtensionFromMimeType(it) }
        clean = "${clean.substringBeforeLast('.')}.${ext ?: "mp3"}"
    }
    val dir = File(ctx.cacheDir, "sound-pick").apply { mkdirs() }
    val out = File(dir, clean)
    ctx.contentResolver.openInputStream(uri)!!.use { input ->
        out.outputStream().use { o ->
            val buf = ByteArray(32 * 1024)
            var total = 0L
            while (true) {
                val n = input.read(buf)
                if (n < 0) break
                total += n
                if (total > MAX_SOUND_BYTES) {
                    o.close(); out.delete()
                    throw IllegalStateException("That file is too big for an alert sound (5 MB at most).")
                }
                o.write(buf, 0, n)
            }
        }
    }
    out
}

/** The shown name of a stored sound file (the core drops the hash it adds). */
@Composable
private fun soundLabel(file: String): String {
    var label by remember(file) { mutableStateOf(file) }
    LaunchedEffect(file) { if (file.isNotEmpty()) runCatching { (Core.call("sounds.label", jo("file" to file)) as? JsonPrimitive)?.contentOrNull?.let { label = it } } }
    return label
}

// ---- the screen --------------------------------------------------------------------------------------------------------

private fun newRuleId() = UUID.randomUUID().toString().take(12)

/** Settings -> Highlights: your name, the alert sound, and the rules (add, edit, reorder, delete). */
@Composable
fun HighlightsSettings(vm: ChatViewModel, onBack: () -> Unit) {
    val s = vm.settings
    val meta = vm.settingsMeta
    val rules = s.list("highlightRules")
    var soundError by remember { mutableStateOf<String?>(null) }
    val pickDefault = rememberSoundPicker({ soundError = null; vm.patchSettings(jo("highlightSoundFile" to it)) }, { soundError = it })
    val colors = (meta["ruleColors"] as? JsonArray)?.mapNotNull { it.jsonPrimitive.contentOrNull } ?: emptyList()
    val badges = (meta["badges"] as? JsonArray)?.mapNotNull { it as? JsonObject } ?: emptyList()
    val badgeColors = (meta["badgeColors"] as? JsonArray)?.mapNotNull { it.jsonPrimitive.contentOrNull } ?: emptyList()

    fun save(list: List<JsonObject>) = vm.patchSettings(jo("highlightRules" to list))
    fun blank(color: String) = jo("id" to newRuleId(), "enabled" to true, "kind" to "phrase", "pattern" to "", "caseSensitive" to false, "color" to color, "sound" to false, "soundFile" to "", "flash" to false)

    SettingsPage("Highlights", onBack) {
        SettingsHeading("Your name")
        SettingsNote("Twitch messages that mention your name are always highlighted in red.")
        SettingsCard {
            SwitchRow("Play a sound", null, s.bool("mentionSound")) { vm.patchSettings(jo("mentionSound" to it)) }
            SwitchRow("Vibrate", "Only while Agu Chat isn't on screen", s.bool("mentionFlash", true)) { vm.patchSettings(jo("mentionFlash" to it)) }
        }
        SettingsHeading("Alert sound")
        SettingsCard {
            val file = s.str("highlightSoundFile")
            Column(Modifier.padding(horizontal = 16.dp, vertical = 10.dp)) {
                Text("Default sound", color = AguColors.Text, fontSize = 15.sp)
                Text("Used by rules and mentions that don't pick their own file", color = AguColors.Muted, fontSize = 12.sp)
                Text(if (file.isEmpty()) "Built-in ping" else soundLabel(file), color = AguColors.Muted, fontSize = 14.sp, modifier = Modifier.padding(top = 4.dp))
                Row(Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Chip("Choose file…") { pickDefault() }
                    if (file.isNotEmpty()) Chip("Use ping") { vm.patchSettings(jo("highlightSoundFile" to "")) }
                    Chip("Play") { vm.playSound(file) }
                }
            }
            SliderRow("Volume", null, s.int("highlightVolume", 70).toFloat(), 0f, 100f, 5f, "%") { vm.patchSettings(jo("highlightVolume" to it.toInt())) }
            SwitchRow("Play sounds while Agu Chat is open", "Off: sounds only play when it's in the background", s.bool("highlightSoundFocused", true)) { vm.patchSettings(jo("highlightSoundFocused" to it)) }
        }
        soundError?.let { Text(it, color = AguColors.Danger, fontSize = 13.sp) }

        Row(Modifier.fillMaxWidth().padding(top = 10.dp), verticalAlignment = Alignment.CenterVertically) {
            Text("Rules", color = AguColors.Text, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Chip("Add badge rules") {
                    val add = badges.mapIndexedNotNull { i, b ->
                        val id = b.str("id")
                        if (rules.any { it.str("kind") == "badge" && it.str("pattern").trim().equals(id, ignoreCase = true) }) null
                        else jo("id" to newRuleId(), "enabled" to true, "kind" to "badge", "pattern" to id, "caseSensitive" to false, "color" to (badgeColors.getOrNull(i) ?: "#9147ff"), "sound" to false, "soundFile" to "", "flash" to false)
                    }
                    save(rules + add)
                }
                Chip("Add rule", on = true) {
                    val used = rules.map { it.str("color").lowercase() }
                    save(rules + blank(colors.firstOrNull { it !in used } ?: colors.getOrElse(rules.size % maxOf(colors.size, 1)) { "#ff9f1a" }))
                }
            }
        }
        SettingsNote("For every platform. Checked top to bottom: the first rule that matches colours the message. Badge rules come last, after every phrase, regex and user rule, and don't turn the tab red. Your own messages are never highlighted.")
        if (rules.isEmpty()) {
            Box(Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).border(1.dp, AguColors.Surface2, RoundedCornerShape(14.dp)).padding(16.dp), contentAlignment = Alignment.Center) {
                Text("No rules yet. Add one to highlight a word, a regular expression or a user.", color = AguColors.Muted, fontSize = 14.sp)
            }
        }
        rules.forEachIndexed { i, r ->
            RuleCard(
                vm, r, first = i == 0, last = i == rules.lastIndex, colors = colors, badges = badges,
                onChange = { next -> save(rules.map { if (it.str("id") == r.str("id")) next else it }) },
                onMove = { d -> val j = i + d; if (j in rules.indices) save(rules.toMutableList().also { l -> val t = l[i]; l[i] = l[j]; l[j] = t }) },
                onDelete = { save(rules.filter { it.str("id") != r.str("id") }) },
            )
        }
        if (rules.isNotEmpty()) RuleTester(vm, rules)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun RuleCard(
    vm: ChatViewModel, r: JsonObject, first: Boolean, last: Boolean, colors: List<String>, badges: List<JsonObject>,
    onChange: (JsonObject) -> Unit, onMove: (Int) -> Unit, onDelete: () -> Unit,
) {
    val kind = r.str("kind", "phrase")
    val enabled = r.bool("enabled", true)
    val color = parseHex(r.str("color", "#9147ff"))
    var colorOpen by remember { mutableStateOf(false) }
    var soundError by remember { mutableStateOf<String?>(null) }
    var draft by remember(r.str("id")) { mutableStateOf(r.str("pattern")) }
    var reError by remember { mutableStateOf<String?>(null) }
    val cs = r.bool("caseSensitive")
    LaunchedEffect(kind, draft, cs) {
        reError = if (kind == "regex" && draft.isNotBlank()) {
            runCatching { (Core.call("settings.check", jo("what" to "regex", "pattern" to draft, "caseSensitive" to cs)) as? JsonObject)?.get("error")?.let { (it as? JsonPrimitive)?.contentOrNull } }.getOrNull()
        } else null
    }
    val pick = rememberSoundPicker({ soundError = null; onChange(r.with("soundFile" to it)) }, { soundError = it })

    Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min).clip(RoundedCornerShape(14.dp)).background(AguColors.Surface)) {
        Box(Modifier.width(5.dp).fillMaxHeight().background(color))
        Column(Modifier.weight(1f).padding(start = 8.dp, end = 10.dp, top = 8.dp, bottom = 10.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                androidx.compose.material3.Switch(enabled, { onChange(r.with("enabled" to it)) }, colors = switchColors())
                ChoiceBox(kind, KINDS, { k ->
                    var next = r.with("kind" to k)
                    if (k == "badge" && badges.none { it.str("id") == r.str("pattern") }) next = next.with("pattern" to badges.firstOrNull()?.str("id").orEmpty())
                    else if (k != "badge" && kind == "badge") next = next.with("pattern" to "")
                    draft = next.str("pattern")
                    onChange(next)
                })
                Box(Modifier.weight(1f)) {}
                Box(Modifier.size(34.dp).clip(RoundedCornerShape(10.dp)).background(color).border(1.dp, AguColors.Muted.copy(alpha = 0.5f), RoundedCornerShape(10.dp)).clickable { colorOpen = true })
            }
            Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                if (kind == "badge") {
                    ChoiceBox(r.str("pattern"), badges.map { it.str("id") to it.str("label") }, { onChange(r.with("pattern" to it)) }, Modifier.weight(1f))
                } else {
                    CommitField(r.str("pattern"), { onChange(r.with("pattern" to it)) }, Modifier.weight(1f), placeholder = placeholderOf(kind), mono = true, invalid = reError != null, onDraft = { draft = it })
                }
                GlyphButton("↑", enabled = !first) { onMove(-1) }
                GlyphButton("↓", enabled = !last) { onMove(1) }
                GlyphButton("✕", danger = true, onClick = onDelete)
            }
            reError?.let { Text("Invalid regular expression: $it", color = AguColors.Danger, fontSize = 12.sp, modifier = Modifier.padding(top = 4.dp)) }
            FlowRow(Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                if (kind != "badge") Chip("Case sensitive", on = cs) { onChange(r.with("caseSensitive" to !cs)) }
                Chip("Vibrate", on = r.bool("flash")) { onChange(r.with("flash" to !r.bool("flash"))) }
                Chip("Sound", on = r.bool("sound")) { onChange(r.with("sound" to !r.bool("sound"))) }
            }
            if (r.bool("sound")) {
                val file = r.str("soundFile")
                val custom = file.isNotEmpty()
                Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    ChoiceBox(if (custom) "file" else "default", listOf("default" to "Default sound", "file" to (if (custom) soundLabel(file) else "Choose file…")), {
                        if (it == "default") onChange(r.with("soundFile" to "")) else pick()
                    })
                    if (custom) Chip("Change") { pick() }
                    Chip("Play") { vm.playSound(if (custom) file else vm.settings.str("highlightSoundFile")) }
                }
                soundError?.let { Text(it, color = AguColors.Danger, fontSize = 12.sp, modifier = Modifier.padding(top = 4.dp)) }
            }
        }
    }
    if (colorOpen) ColorDialog(r.str("color"), colors, onPick = { colorOpen = false; onChange(r.with("color" to it)) }, onDismiss = { colorOpen = false })
}

/** The desktop's palette, and any #rrggbb. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ColorDialog(current: String, colors: List<String>, onPick: (String) -> Unit, onDismiss: () -> Unit) {
    var hex by remember { mutableStateOf(current) }
    val ok = Regex("^#?[0-9a-fA-F]{6}$").matches(hex.trim())
    AlertDialog(
        onDismissRequest = onDismiss, containerColor = AguColors.Surface,
        title = { Text("Highlight colour", color = AguColors.Text) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                FlowRow(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    for (c in colors) Box(
                        Modifier.size(40.dp).clip(RoundedCornerShape(10.dp)).background(parseHex(c))
                            .border(if (c.equals(current, true)) 3.dp else 1.dp, if (c.equals(current, true)) Color.White else AguColors.Muted.copy(alpha = 0.5f), RoundedCornerShape(10.dp))
                            .clickable { onPick(c) },
                    )
                }
                DraftField(hex, { hex = it }, placeholder = "#rrggbb")
            }
        },
        confirmButton = { TextButton(enabled = ok, onClick = { onPick("#" + hex.trim().trimStart('#').lowercase()) }) { Text("Use this colour", color = if (ok) AguColors.Accent else AguColors.Muted) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel", color = AguColors.Muted) } },
    )
}

/** "Try it": which rule a sample message would hit, as on the desktop. */
@Composable
private fun RuleTester(vm: ChatViewModel, rules: List<JsonObject>) {
    var user by remember { mutableStateOf("") }
    var text by remember { mutableStateOf("") }
    var result by remember { mutableStateOf<JsonObject?>(null) }
    LaunchedEffect(user, text, rules) {
        result = if (user.isEmpty() && text.isEmpty()) null else runCatching { Core.call("highlights.test", jo("user" to user, "text" to text)) as? JsonObject }.getOrNull()
    }
    SettingsCard(Modifier.padding(12.dp)) {
        Text("TRY IT", color = AguColors.Muted, fontSize = 11.sp, modifier = Modifier.padding(bottom = 6.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            DraftField(user, { user = it }, Modifier.weight(0.4f), placeholder = "User")
            DraftField(text, { text = it }, Modifier.weight(0.6f), placeholder = "A sample message")
        }
        val r = result
        if (r != null) {
            val hit = (r["index"] as? JsonPrimitive)?.contentOrNull?.toIntOrNull()
            if (hit != null) {
                Text(
                    "Rule ${hit + 1} (${r.str("kind")} ${r.str("pattern")}) highlights this.",
                    color = AguColors.Text, fontSize = 13.sp, fontFamily = FontFamily.Default,
                    modifier = Modifier.padding(top = 8.dp).clip(RoundedCornerShape(6.dp)).background(parseHex(r.str("color")).copy(alpha = 0.28f)).padding(horizontal = 8.dp, vertical = 4.dp),
                )
            } else Text("No rule matches.", color = AguColors.Muted, fontSize = 13.sp, modifier = Modifier.padding(top = 8.dp))
        }
    }
}
