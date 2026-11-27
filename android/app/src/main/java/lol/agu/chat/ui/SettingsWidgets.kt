package lol.agu.chat.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.floatOrNull
import kotlinx.serialization.json.intOrNull

// ---- the settings object ---------------------------------------------------------------------------------------

internal fun JsonObject.bool(k: String, d: Boolean = false): Boolean = (this[k] as? JsonPrimitive)?.booleanOrNull ?: d
internal fun JsonObject.str(k: String, d: String = ""): String = (this[k] as? JsonPrimitive)?.contentOrNull ?: d
internal fun JsonObject.num(k: String, d: Float = 0f): Float = (this[k] as? JsonPrimitive)?.floatOrNull ?: d
internal fun JsonObject.int(k: String, d: Int = 0): Int = (this[k] as? JsonPrimitive)?.intOrNull ?: d
internal fun JsonObject.list(k: String): List<JsonObject> = (this[k] as? JsonArray)?.mapNotNull { it as? JsonObject } ?: emptyList()
internal fun JsonObject.with(vararg p: Pair<String, Any?>): JsonObject = JsonObject(this + p.associate { it.first to jsonOf(it.second) })

internal fun jsonOf(v: Any?): JsonElement = when (v) {
    null -> kotlinx.serialization.json.JsonNull
    is JsonElement -> v
    is Boolean -> JsonPrimitive(v)
    is Number -> JsonPrimitive(v)
    is String -> JsonPrimitive(v)
    is List<*> -> JsonArray(v.map { jsonOf(it) })
    else -> JsonPrimitive(v.toString())
}

internal fun jo(vararg p: Pair<String, Any?>): JsonObject = JsonObject(p.associate { it.first to jsonOf(it.second) })

internal fun parseHex(hex: String, fallback: Color = AguColors.Accent): Color =
    runCatching { Color(android.graphics.Color.parseColor(hex)) }.getOrDefault(fallback)

// ---- page and rows -----------------------------------------------------------------------------------------------

/** A settings page: the back arrow and title, then the content, scrolling. */
@Composable
fun SettingsPage(title: String, onBack: () -> Unit, content: @Composable ColumnScope.() -> Unit) {
    BackHandler(onBack = onBack)
    Column(Modifier.fillMaxSize().background(AguColors.Bg).statusBarsPadding().navigationBarsPadding().imePadding()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                "←", color = AguColors.Text, fontSize = 24.sp,
                modifier = Modifier.clip(CircleShape).clickable(onClick = onBack).padding(horizontal = 14.dp, vertical = 6.dp),
            )
            Text(title, color = AguColors.Text, fontSize = 20.sp, fontWeight = FontWeight.Bold)
        }
        Column(
            Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp).padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
            content = content,
        )
    }
}

@Composable
fun SettingsHeading(text: String) {
    Text(text, color = AguColors.Text, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(top = 10.dp))
}

@Composable
fun SettingsNote(text: String) {
    Text(text, color = AguColors.Muted, fontSize = 12.sp, lineHeight = 17.sp)
}

/** A rounded block of rows. */
@Composable
fun SettingsCard(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(AguColors.Surface).then(modifier), content = content)
}

@Composable
private fun RowText(label: String, hint: String?, enabled: Boolean, modifier: Modifier) {
    Column(modifier) {
        Text(label, color = if (enabled) AguColors.Text else AguColors.Muted, fontSize = 15.sp)
        if (hint != null) Text(hint, color = AguColors.Muted, fontSize = 12.sp, lineHeight = 16.sp)
    }
}

/** A setting with a switch: the whole row toggles it. */
@Composable
fun SwitchRow(label: String, hint: String?, checked: Boolean, enabled: Boolean = true, onChange: (Boolean) -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(enabled = enabled) { onChange(!checked) }.padding(horizontal = 16.dp, vertical = 11.dp),
        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        RowText(label, hint, enabled, Modifier.weight(1f))
        Switch(checked, null, enabled = enabled, colors = switchColors())
    }
}

@Composable
fun switchColors() = SwitchDefaults.colors(
    checkedThumbColor = Color.White, checkedTrackColor = AguColors.Accent,
    uncheckedThumbColor = AguColors.Muted, uncheckedTrackColor = AguColors.Surface2, uncheckedBorderColor = AguColors.Muted,
)

/** A setting with a list of choices: shows the current one, a tap opens the list. */
@Composable
fun ChoiceRow(label: String, hint: String?, current: String, options: List<Pair<String, String>>, onPick: (String) -> Unit) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        RowText(label, hint, true, Modifier.weight(1f))
        ChoiceBox(current, options, onPick)
    }
}

/** A small drop-down: the current choice with an arrow. */
@Composable
fun ChoiceBox(current: String, options: List<Pair<String, String>>, onPick: (String) -> Unit, modifier: Modifier = Modifier, invalid: Boolean = false) {
    var open by remember { mutableStateOf(false) }
    Box(modifier) {
        Row(
            Modifier.clip(RoundedCornerShape(10.dp)).background(AguColors.Surface2).border(1.dp, if (invalid) AguColors.Danger else Color.Transparent, RoundedCornerShape(10.dp))
                .clickable { open = true }.padding(horizontal = 12.dp, vertical = 9.dp),
            verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(options.firstOrNull { it.first == current }?.second ?: current, color = AguColors.Text, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f, fill = false))
            Text("▾", color = AguColors.Muted, fontSize = 12.sp)
        }
        DropdownMenu(open, { open = false }, containerColor = AguColors.Surface2) {
            for ((id, name) in options) {
                DropdownMenuItem(
                    text = { Text(name, color = if (id == current) AguColors.Accent else AguColors.Text, fontSize = 14.sp) },
                    onClick = { open = false; onPick(id) },
                )
            }
        }
    }
}

/** A number setting on a slider; it saves when the finger lets go. */
@Composable
fun SliderRow(label: String, hint: String?, value: Float, min: Float, max: Float, step: Float, suffix: String, onCommit: (Float) -> Unit) {
    var drag by remember { mutableFloatStateOf(value) }
    var dragging by remember { mutableStateOf(false) }
    val shown = if (dragging) drag else value
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            RowText(label, hint, true, Modifier.weight(1f))
            Text("${formatNumber(shown)}$suffix", color = AguColors.Text, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
        }
        Slider(
            value = shown, onValueChange = { dragging = true; drag = it }, valueRange = min..max,
            steps = (((max - min) / step).toInt() - 1).coerceAtLeast(0),
            onValueChangeFinished = { dragging = false; onCommit(drag) },
            colors = SliderDefaults.colors(thumbColor = AguColors.Accent, activeTrackColor = AguColors.Accent, inactiveTrackColor = AguColors.Surface2, activeTickColor = Color.Transparent, inactiveTickColor = Color.Transparent),
        )
    }
}

private fun formatNumber(v: Float) = if (v % 1f == 0f) v.toInt().toString() else "%.1f".format(v)

@Composable
fun Divider() {
    Box(Modifier.fillMaxWidth().padding(horizontal = 16.dp).heightIn(min = 1.dp).background(AguColors.Surface2))
}

/** A small round-cornered text button. */
@Composable
fun Chip(text: String, on: Boolean = false, enabled: Boolean = true, danger: Boolean = false, onClick: () -> Unit) {
    val fg = when {
        !enabled -> AguColors.Muted.copy(alpha = 0.5f)
        danger -> AguColors.Danger
        on -> Color.White
        else -> AguColors.Text
    }
    Box(
        Modifier.clip(RoundedCornerShape(10.dp)).background(if (on) AguColors.Accent else AguColors.Surface2).clickable(enabled = enabled, onClick = onClick).padding(horizontal = 12.dp, vertical = 9.dp),
        contentAlignment = Alignment.Center,
    ) { Text(text, color = fg, fontSize = 13.sp, fontWeight = FontWeight.Medium) }
}

/** A solid accent button. */
@Composable
fun AccentButton(text: String, enabled: Boolean = true, onClick: () -> Unit) {
    Box(
        Modifier.clip(RoundedCornerShape(12.dp)).background(if (enabled) AguColors.Accent else AguColors.Surface2).clickable(enabled = enabled, onClick = onClick).padding(horizontal = 18.dp, vertical = 11.dp),
        contentAlignment = Alignment.Center,
    ) { Text(text, color = if (enabled) Color.White else AguColors.Muted, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center) }
}

/** A tappable glyph (the arrows, the bin). */
@Composable
fun GlyphButton(glyph: String, enabled: Boolean = true, danger: Boolean = false, onClick: () -> Unit) {
    Box(
        Modifier.size(36.dp).clip(CircleShape).clickable(enabled = enabled, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Text(glyph, color = if (!enabled) AguColors.Muted.copy(alpha = 0.4f) else if (danger) AguColors.Danger else AguColors.Text, fontSize = 17.sp)
    }
}

// ---- text fields ---------------------------------------------------------------------------------------------------

/**
 * A text field that saves what is typed after a short pause, when focus leaves it, or on Done; the saved value shows
 * again whenever it changes from outside and the field is not being typed in.
 */
@Composable
fun CommitField(
    value: String, onCommit: (String) -> Unit, modifier: Modifier = Modifier, placeholder: String = "", mono: Boolean = false,
    invalid: Boolean = false, keyboard: KeyboardType = KeyboardType.Text, ime: ImeAction = ImeAction.Done, onDone: () -> Unit = {},
    commitOnPause: Boolean = true, onDraft: (String) -> Unit = {},
) {
    var text by remember { mutableStateOf(value) }
    var focused by remember { mutableStateOf(false) }
    var seen by remember { mutableStateOf(value) }
    if (value != seen) {
        seen = value
        if (!focused) text = value
    }
    val current by rememberUpdatedState(value)
    val commit by rememberUpdatedState(onCommit)
    if (commitOnPause) {
        LaunchedEffect(text) {
            if (text != current) {
                delay(700)
                if (text != current) commit(text)
            }
        }
    }
    BasicTextField(
        text, { text = it; onDraft(it) }, singleLine = true,
        textStyle = TextStyle(color = AguColors.Text, fontSize = 14.sp, fontFamily = if (mono) FontFamily.Monospace else FontFamily.Default),
        cursorBrush = SolidColor(AguColors.Accent),
        keyboardOptions = KeyboardOptions(keyboardType = keyboard, imeAction = ime, autoCorrectEnabled = false),
        keyboardActions = KeyboardActions(onDone = { if (text != current) onCommit(text); onDone() }, onNext = { if (text != current) onCommit(text) }),
        modifier = modifier.onFocusChanged {
            if (focused && !it.isFocused && text != current) onCommit(text)
            focused = it.isFocused
        },
        decorationBox = { inner ->
            Box(
                Modifier.fillMaxWidth().heightIn(min = 40.dp).clip(RoundedCornerShape(10.dp)).background(AguColors.Surface2)
                    .border(1.dp, if (invalid) AguColors.Danger else Color.Transparent, RoundedCornerShape(10.dp)).padding(horizontal = 12.dp, vertical = 10.dp),
                contentAlignment = Alignment.CenterStart,
            ) {
                if (text.isEmpty()) Text(placeholder, color = AguColors.Muted, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                inner()
            }
        },
    )
}

/** A plain draft field (nothing is saved until the caller does). */
@Composable
fun DraftField(value: String, onChange: (String) -> Unit, modifier: Modifier = Modifier, placeholder: String = "", ime: ImeAction = ImeAction.Done, onDone: () -> Unit = {}) {
    BasicTextField(
        value, onChange, singleLine = true,
        textStyle = TextStyle(color = AguColors.Text, fontSize = 14.sp),
        cursorBrush = SolidColor(AguColors.Accent),
        keyboardOptions = KeyboardOptions(imeAction = ime, autoCorrectEnabled = false),
        keyboardActions = KeyboardActions(onDone = { onDone() }),
        modifier = modifier,
        decorationBox = { inner ->
            Box(
                Modifier.fillMaxWidth().heightIn(min = 40.dp).clip(RoundedCornerShape(10.dp)).background(AguColors.Surface2).padding(horizontal = 12.dp, vertical = 10.dp),
                contentAlignment = Alignment.CenterStart,
            ) {
                if (value.isEmpty()) Text(placeholder, color = AguColors.Muted, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                inner()
            }
        },
    )
}

