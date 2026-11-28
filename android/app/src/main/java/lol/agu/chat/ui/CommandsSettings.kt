package lol.agu.chat.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonObject
import lol.agu.chat.Core

/** The biggest commands file the import reads (a Chatterino one is a few kilobytes). */
private const val MAX_IMPORT_BYTES = 1_000_000

/** A command being added (index -1) or edited. */
private data class Editing(val index: Int, val name: String, val text: String, val inMenu: Boolean)

/**
 * Settings -> Commands: a short name typed in the message box sends saved text (the desktop's page, same words). The
 * rules (names, placeholders, what an import takes) are the core's; this is the list, the editor and the file picker.
 */
@Composable
fun CommandsSettings(vm: ChatViewModel, onBack: () -> Unit) {
    val list = vm.settings.list("customCommands")
    var editing by remember { mutableStateOf<Editing?>(null) }
    var status by remember { mutableStateOf<Pair<String, Boolean>?>(null) }
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()

    fun save(next: List<JsonObject>) = vm.patchSettings(jo("customCommands" to next))

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val text = withContext(Dispatchers.IO) {
                runCatching {
                    ctx.contentResolver.openInputStream(uri)?.use { it.readNBytes(MAX_IMPORT_BYTES + 1) }?.takeIf { it.size <= MAX_IMPORT_BYTES }?.toString(Charsets.UTF_8)
                }.getOrNull()
            }
            status = if (text == null) "Could not read that file" to true
            else runCatching { Core.call("commands.import", jo("text" to text)) as JsonObject }
                .map { it.str("status") to it.bool("warn") }
                .getOrElse { (it.message ?: "Could not import") to true }
        }
    }

    SettingsPage("Commands", onBack) {
        SettingsNote(vm.settingsMeta.str("commandsHelp"))
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            AccentButton("Add command") { editing = Editing(-1, "", "", false) }
            Chip("Import from file…") { picker.launch(arrayOf("application/json", "text/*", "application/octet-stream", "*/*")) }
        }
        status?.let { (t, warn) -> Text(t, color = if (warn) AguColors.Danger else AguColors.Muted, fontSize = 13.sp) }
        SettingsCard {
            SwitchRow("Commands in the message menu", "The commands marked In menu, under a message's actions", vm.settings.bool("commandsButton", true)) { vm.patchSettings(jo("commandsButton" to it)) }
        }
        SettingsCard {
            if (list.isEmpty()) Text("No custom commands.", color = AguColors.Muted, fontSize = 15.sp, modifier = Modifier.padding(16.dp))
            list.forEachIndexed { i, c ->
                Row(
                    Modifier.fillMaxWidth().clickable { editing = Editing(i, c.str("name"), c.str("text"), c.bool("inMenu")) }.padding(horizontal = 16.dp, vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(Modifier.weight(1f)) {
                        Text(c.str("name"), color = AguColors.Text, fontSize = 16.sp, fontWeight = androidx.compose.ui.text.font.FontWeight.Medium)
                        Text(
                            c.str("text").lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() }.orEmpty(),
                            color = AguColors.Muted, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                        )
                    }
                    if (c.bool("inMenu")) Text("In menu", color = AguColors.Accent, fontSize = 12.sp, modifier = Modifier.padding(start = 8.dp))
                    Text("›", color = AguColors.Muted, fontSize = 20.sp, modifier = Modifier.padding(start = 10.dp))
                }
                if (i < list.lastIndex) Divider()
            }
        }
    }

    editing?.let { e ->
        CommandEditor(
            e,
            onSave = { name, text, inMenu ->
                val entry = jo("name" to name, "text" to text, "inMenu" to inMenu)
                save(if (e.index < 0) list + entry else list.mapIndexed { i, c -> if (i == e.index) entry else c })
                editing = null
            },
            onDelete = if (e.index >= 0) ({ save(list.filterIndexed { i, _ -> i != e.index }); editing = null }) else null,
            onDismiss = { editing = null },
        )
    }
}

@Composable
private fun CommandEditor(e: Editing, onSave: (String, String, Boolean) -> Unit, onDelete: (() -> Unit)?, onDismiss: () -> Unit) {
    var name by remember { mutableStateOf(e.name) }
    var text by remember { mutableStateOf(e.text) }
    var inMenu by remember { mutableStateOf(e.inMenu) }
    var error by remember { mutableStateOf<String?>(null) }
    var cleaned by remember { mutableStateOf(e.name) }
    var touched by remember { mutableStateOf(false) }
    // the core says why it can't be saved, and what the name is saved as
    LaunchedEffect(name, text) {
        val r = runCatching { Core.call("commands.check", jo("name" to name, "text" to text, "index" to if (e.index >= 0) e.index else null)) as? JsonObject }.getOrNull()
        error = r?.str("error")?.ifEmpty { null }
        cleaned = r?.str("name").orEmpty().ifEmpty { name }
    }
    val field = TextStyle(color = AguColors.Text, fontSize = 15.sp)
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = AguColors.Surface,
        title = { Text(if (e.index < 0) "New command" else "Edit command", color = AguColors.Text) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                BasicTextField(
                    name, { name = it.filter { c -> !c.isWhitespace() }; touched = true }, singleLine = true, textStyle = field, cursorBrush = SolidColor(AguColors.Accent),
                    keyboardOptions = KeyboardOptions(autoCorrectEnabled = false, capitalization = KeyboardCapitalization.None),
                    decorationBox = { inner -> FieldBox("/name", name.isEmpty(), inner) },
                )
                BasicTextField(
                    text, { text = it; touched = true }, textStyle = field, cursorBrush = SolidColor(AguColors.Accent), minLines = 3, maxLines = 8,
                    keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
                    decorationBox = { inner -> FieldBox("Text to send", text.isEmpty(), inner) },
                )
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Show in message menu", color = AguColors.Text, fontSize = 15.sp, modifier = Modifier.weight(1f))
                    Switch(inMenu, { inMenu = it }, colors = switchColors())
                }
                if (touched && error != null) Text(error!!, color = AguColors.Danger, fontSize = 13.sp)
            }
        },
        confirmButton = {
            TextButton(onClick = { touched = true; if (error == null) onSave(cleaned, text, inMenu) }) { Text(if (e.index < 0) "Add" else "Save", color = if (error == null) AguColors.Accent else AguColors.Muted) }
        },
        dismissButton = {
            Row {
                if (onDelete != null) TextButton(onClick = onDelete) { Text("Delete", color = AguColors.Danger) }
                TextButton(onClick = onDismiss) { Text("Cancel", color = AguColors.Muted) }
            }
        },
    )
}

@Composable
private fun FieldBox(placeholder: String, empty: Boolean, inner: @Composable () -> Unit) {
    Box(
        Modifier.fillMaxWidth().heightIn(min = 40.dp).clip(RoundedCornerShape(10.dp)).background(AguColors.Surface2).padding(horizontal = 12.dp, vertical = 10.dp),
        contentAlignment = Alignment.TopStart,
    ) {
        if (empty) Text(placeholder, color = AguColors.Muted, fontSize = 15.sp)
        inner()
    }
}
