package lol.agu.chat.ui

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
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import lol.agu.chat.Core

private val ACTIONS = listOf("ban" to "Ban", "timeout" to "Timeout", "warn" to "Warn", "delete" to "Delete message", "command" to "Command")

private fun defaultIcon(action: String) = when (action) {
    "ban" -> "ban"
    "timeout" -> "clock"
    "warn" -> "warning"
    "delete" -> "trash"
    else -> "chat"
}

private fun buttonJson(b: ModButtonCfg) = jo("id" to b.id, "action" to b.action, "icon" to b.icon, "label" to b.label, "seconds" to b.seconds, "command" to b.command)

private fun newButtonId() = "b" + java.lang.Long.toHexString(System.currentTimeMillis()) + (0..999).random().toString(16)

/** The face of a mod button: its text, or its icon. */
@Composable
fun ModFace(b: JsonObject, color: Color, iconSize: Int = 18) {
    val label = b.str("label")
    if (label.isNotEmpty()) Text(label, color = color, fontSize = 12.sp, fontWeight = FontWeight.Bold)
    else ModIcon(b.str("icon").ifEmpty { "chat" }, color, iconSize)
}

/** Settings -> Mod buttons: the buttons every message gets when mod mode is on (add, remove, reorder, reset). */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun ModButtonsSettings(vm: ChatViewModel, onBack: () -> Unit) {
    val meta = vm.settingsMeta
    val buttons = vm.settings.list("modButtons")
    val max = meta.int("maxModButtons", 12)
    val icons = (meta["modIcons"] as? JsonArray)?.mapNotNull { it as? JsonObject } ?: emptyList()
    val defaults = DefaultModButtons.map { buttonJson(it) }
    val isDefault = buttons == defaults

    fun save(list: List<JsonObject>) = vm.patchSettings(jo("modButtons" to list))

    SettingsPage("Mod buttons", onBack) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.weight(1f))
            Chip("Reset to defaults", enabled = !isDefault) { save(defaults) }
        }
        SettingsNote("Shown in this order at the start of every message when a split's mod mode is on. Ban, timeout and delete work wherever you can moderate; commands run in Twitch chats.")
        // the row preview, like the desktop's
        SettingsCard(Modifier.padding(14.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (b in buttons) Box(Modifier.height(24.dp).width(28.dp), contentAlignment = Alignment.Center) { ModFace(b, AguColors.Muted, 16) }
                Text("viewer", color = AguColors.Purple, fontWeight = FontWeight.SemiBold, fontSize = 15.sp)
                Text(":", color = AguColors.Muted, fontSize = 15.sp)
                Text("example message", color = AguColors.Text, fontSize = 15.sp)
            }
            if (buttons.isEmpty()) Text("No buttons: rows show none in mod mode.", color = AguColors.Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 4.dp))
        }
        buttons.forEachIndexed { i, b ->
            ButtonEditor(
                b, first = i == 0, last = i == buttons.lastIndex, icons = icons, maxLabel = meta.int("maxLabel", 8), templateHelp = meta.str("templateHelp"),
                onChange = { next -> save(buttons.map { if (it.str("id") == b.str("id")) next else it }) },
                onMove = { d -> val j = i + d; if (j in buttons.indices) save(buttons.toMutableList().also { l -> val t = l[i]; l[i] = l[j]; l[j] = t }) },
                onRemove = { save(buttons.filter { it.str("id") != b.str("id") }) },
            )
        }
        Row {
            Chip("Add button", enabled = buttons.size < max) {
                save(buttons + jo("id" to newButtonId(), "action" to "timeout", "icon" to "clock", "label" to "1m", "seconds" to 60, "command" to ""))
            }
        }
        if (buttons.size < max) SettingsNote("Adds a 1 minute timeout; change it below.")
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ButtonEditor(
    b: JsonObject, first: Boolean, last: Boolean, icons: List<JsonObject>, maxLabel: Int, templateHelp: String,
    onChange: (JsonObject) -> Unit, onMove: (Int) -> Unit, onRemove: () -> Unit,
) {
    val action = b.str("action", "ban")
    val seconds = b.int("seconds")
    val scope = rememberCoroutineScope()
    var lengthText by remember(b.str("id"), seconds) { mutableStateOf("${seconds}s") }
    LaunchedEffect(seconds) {
        if (seconds > 0) runCatching { (Core.call("modbtn.duration", jo("seconds" to seconds)) as? JsonObject)?.str("text") }.getOrNull()?.takeIf { it.isNotEmpty() }?.let { lengthText = it }
    }
    var lengthBad by remember { mutableStateOf(false) }
    val face = b

    SettingsCard(Modifier.padding(12.dp)) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Box(Modifier.width(40.dp).height(34.dp).clip(RoundedCornerShape(10.dp)).background(AguColors.Bg).border(1.dp, AguColors.Surface2, RoundedCornerShape(10.dp)), contentAlignment = Alignment.Center) {
                ModFace(face, AguColors.Text, 18)
            }
            ChoiceBox(action, ACTIONS, { a ->
                // a new action gets that action's look (a "1m" label would mislabel a ban)
                var next = b.with("action" to a, "icon" to defaultIcon(a), "label" to "")
                if (a == "timeout" && seconds == 0) next = next.with("seconds" to 600)
                if (a == "command" && b.str("command").isEmpty()) next = next.with("command" to "/warn {user} Please follow the chat rules")
                onChange(next)
            })
            Box(Modifier.weight(1f))
            GlyphButton("↑", enabled = !first) { onMove(-1) }
            GlyphButton("↓", enabled = !last) { onMove(1) }
            GlyphButton("✕", danger = true, onClick = onRemove)
        }
        when (action) {
            "timeout" -> Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                CommitField(
                    lengthText, { text ->
                        scope.launch {
                            val s = runCatching { (Core.call("modbtn.duration", jo("text" to text)) as? JsonObject)?.get("seconds")?.let { (it as? JsonPrimitive)?.intOrNull } }.getOrNull()
                            lengthBad = s == null
                            if (s != null && s != seconds) onChange(b.with("seconds" to s))
                        }
                    },
                    Modifier.width(96.dp), placeholder = "10m", invalid = lengthBad,
                )
                Text("30s, 10m, 1h, 1d, up to 2w", color = AguColors.Muted, fontSize = 12.sp)
            }
            "command" -> Column(Modifier.padding(top = 8.dp)) {
                CommitField(b.str("command"), { t -> if (t.isNotBlank()) onChange(b.with("command" to t.trim().take(500))) }, placeholder = "/timeout {user} 600", invalid = b.str("command").isBlank())
                Text("A slash command or a chat message. Placeholders: $templateHelp.", color = AguColors.Muted, fontSize = 12.sp, lineHeight = 16.sp, modifier = Modifier.padding(top = 4.dp))
            }
        }
        Text("Icon", color = AguColors.Muted, fontSize = 12.sp, modifier = Modifier.padding(top = 10.dp, bottom = 4.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            for (ic in icons) {
                val on = b.str("label").isEmpty() && b.str("icon") == ic.str("id")
                Box(
                    Modifier.size(38.dp).clip(RoundedCornerShape(10.dp)).background(if (on) AguColors.Accent.copy(alpha = 0.2f) else Color.Transparent)
                        .border(1.dp, if (on) AguColors.Accent else AguColors.Surface2, RoundedCornerShape(10.dp))
                        .clickable { onChange(b.with("icon" to ic.str("id"), "label" to "")) },
                    contentAlignment = Alignment.Center,
                ) { ModIcon(ic.str("id"), if (on) AguColors.Text else AguColors.Muted, 18) }
            }
        }
        Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("or text", color = AguColors.Muted, fontSize = 12.sp)
            CommitField(
                b.str("label"), { t ->
                    val label = t.take(maxLabel)
                    if (label != b.str("label")) {
                        var next = b.with("label" to label)
                        if (label.isBlank()) next = next.with("label" to "", "icon" to b.str("icon").ifEmpty { defaultIcon(action) })
                        onChange(next)
                    }
                },
                Modifier.width(96.dp), placeholder = "1d",
            )
        }
    }
}
