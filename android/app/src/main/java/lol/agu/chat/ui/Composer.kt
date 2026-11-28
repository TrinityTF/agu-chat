package lol.agu.chat.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Who a reply is to: the name shown and the start of what they said. */
data class ReplyBar(val user: String, val text: String)

/**
 * The message box, as the desktop's: a strip of suggestions while you type (`:` emotes, `@` people, `/` commands, a
 * word's start), the emote button, who the message goes to, the reply bar, the /clear and /raid confirm, the chat's
 * mode hints, and Send (long-press for what you sent before). A chat you can't write in shows why instead.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun Composer(
    value: TextFieldValue,
    onValue: (TextFieldValue) -> Unit,
    /** false: the chat can't be written in, [placeholder] says why */
    canSend: Boolean,
    /** a chat mode keeps this account out: the box locks, [placeholder] says why */
    locked: Boolean,
    placeholder: String,
    showLogin: Boolean,
    onLogin: () -> Unit,
    reply: ReplyBar?,
    onCancelReply: () -> Unit,
    confirm: Confirm?,
    onConfirm: () -> Unit,
    onCancelConfirm: () -> Unit,
    notices: List<Gate>,
    suggestions: Suggestions?,
    onPick: (Suggestion) -> Unit,
    /** the "Recent" chip shows when the box is empty and there is something to bring back */
    hasHistory: Boolean,
    sending: Boolean,
    onSend: () -> Unit,
    onHistory: () -> Unit,
    onEmotes: () -> Unit,
    focus: FocusRequester,
    modifier: Modifier = Modifier,
    /** In a merged tab: who the message goes to ("All" or a channel); null hides the picker. */
    target: SendTargetChip? = null,
    onTarget: () -> Unit = {},
) {
    val writable = canSend && !locked
    Column(modifier.fillMaxWidth().background(AguColors.Bg)) {
        for (g in notices) {
            Row(Modifier.fillMaxWidth().background(AguColors.Surface).padding(horizontal = 12.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                if (g.level != "info") Text("🔒 ", fontSize = 11.sp)
                Text(g.text, color = if (g.level == "info") HintColor else AguColors.Muted, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        }
        if (confirm != null) {
            Row(
                Modifier.fillMaxWidth().background(AguColors.Danger.copy(alpha = 0.12f)).padding(start = 12.dp, end = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(confirm.text, color = Color(0xFFFCA5A5), fontSize = 13.sp, modifier = Modifier.weight(1f).padding(vertical = 8.dp))
                TextButton(onClick = onConfirm, enabled = !sending) { Text("Confirm", color = AguColors.Danger, fontWeight = FontWeight.SemiBold) }
                TextButton(onClick = onCancelConfirm) { Text("Cancel", color = AguColors.Muted) }
            }
        }
        if (reply != null) {
            Row(
                Modifier.fillMaxWidth().background(AguColors.Surface).padding(start = 12.dp, top = 2.dp, bottom = 2.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "↩ Replying to @${reply.user}: ${reply.text}", color = AguColors.Muted, fontSize = 13.sp,
                    maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f),
                )
                Text(
                    "×", color = AguColors.Text, fontSize = 22.sp,
                    modifier = Modifier.clip(CircleShape).clickable(onClick = onCancelReply).padding(horizontal = 14.dp, vertical = 2.dp),
                )
            }
        }
        if (writable) Strip(suggestions, onPick, showRecent = hasHistory && value.text.isEmpty() && suggestions == null, onRecent = onHistory)
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 8.dp),
            verticalAlignment = Alignment.Bottom,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Box(
                Modifier.size(44.dp).clip(CircleShape).background(AguColors.Surface2).alpha(if (writable) 1f else 0.4f)
                    .clickable(enabled = writable, onClick = onEmotes),
                contentAlignment = Alignment.Center,
            ) { SmileIcon(AguColors.Muted) }
            if (target != null) {
                Row(
                    Modifier.heightIn(min = 44.dp).clip(RoundedCornerShape(22.dp)).background(AguColors.Surface2)
                        .clickable(onClick = onTarget).padding(horizontal = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    val shown = if (target.platform != null) listOf(target.platform) else target.platforms.distinct()
                    if (shown.isEmpty()) Text(target.label, color = AguColors.Text, fontSize = 13.sp, maxLines = 1)
                    for (p in shown) PlatformLogo(p, 18.dp)
                    Text("▾", color = AguColors.Muted, fontSize = 11.sp)
                }
            }
            Box(
                Modifier.weight(1f).heightIn(min = 44.dp).clip(RoundedCornerShape(22.dp)).background(AguColors.Surface2)
                    .padding(horizontal = 16.dp, vertical = 10.dp),
                contentAlignment = Alignment.CenterStart,
            ) {
                if (!writable) {
                    Text(placeholder, color = AguColors.Muted, fontSize = 15.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                } else {
                    if (value.text.isEmpty()) Text(placeholder, color = AguColors.Muted, fontSize = 15.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    BasicTextField(
                        value = value,
                        onValueChange = onValue,
                        textStyle = TextStyle(color = AguColors.Text, fontSize = 15.sp),
                        cursorBrush = SolidColor(AguColors.Accent),
                        maxLines = 4,
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                        keyboardActions = KeyboardActions(onSend = { onSend() }),
                        modifier = Modifier.fillMaxWidth().focusRequester(focus),
                    )
                }
            }
            if (writable) {
                val enabled = value.text.isNotBlank() && !sending
                Box(
                    Modifier.size(44.dp).clip(CircleShape)
                        .background(if (enabled) AguColors.Accent else AguColors.Surface2)
                        .combinedClickable(onClick = { if (enabled) onSend() }, onLongClick = onHistory),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(if (sending) "…" else "↑", color = if (enabled) Color.White else AguColors.Muted, fontSize = 22.sp, textAlign = TextAlign.Center)
                }
            } else if (showLogin) {
                TextButton(onClick = onLogin) { Text("Log in", color = AguColors.Accent) }
            }
        }
        val n = value.text.codePointCount(0, value.text.length)
        if (writable && n >= MAX_CHARS - 50) {
            Text("$n/$MAX_CHARS", color = if (n >= MAX_CHARS) AguColors.Danger else HintColor, fontSize = 11.sp, textAlign = TextAlign.End, modifier = Modifier.fillMaxWidth().padding(end = 18.dp, bottom = 4.dp))
        }
    }
}

internal val HintColor = Color(0xFF8B8B94)

/** The suggestions above the keyboard: one chip each; tap to put it in the text. */
@Composable
private fun Strip(s: Suggestions?, onPick: (Suggestion) -> Unit, showRecent: Boolean, onRecent: () -> Unit) {
    if (s == null && !showRecent) return
    LazyRow(
        Modifier.fillMaxWidth().background(AguColors.Surface).padding(vertical = 6.dp),
        contentPadding = PaddingValues(horizontal = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (s == null) {
            item {
                Text(
                    "↺ Recent", color = AguColors.Text, fontSize = 14.sp,
                    modifier = Modifier.heightIn(min = 40.dp).clip(RoundedCornerShape(20.dp)).background(AguColors.Surface2).clickable(onClick = onRecent).padding(horizontal = 14.dp, vertical = 10.dp),
                )
            }
        } else items(s.items) { it ->
            Row(
                Modifier.heightIn(min = 40.dp).clip(RoundedCornerShape(20.dp)).background(AguColors.Surface2).alpha(if (it.dim) 0.5f else 1f)
                    .clickable { onPick(it) }.padding(horizontal = 12.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (it.img.isNotEmpty()) EmoteImage(it.img, LocalDisplay.current.animatedEmotes, Modifier.size(28.dp))
                Column(Modifier.widthIn(max = 230.dp)) {
                    Text(it.name, color = if (s.kind == "command") AguColors.Accent else AguColors.Text, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    if (it.hint.isNotEmpty()) Text(if (it.dim) "${it.hint} (not available)" else it.hint, color = HintColor, fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
        }
    }
}

/** The emote button's face: a smile. */
@Composable
private fun SmileIcon(color: Color) {
    Canvas(Modifier.size(24.dp)) {
        val w = size.width
        val sw = w * 0.09f
        drawCircle(color, radius = w * 0.42f, center = Offset(w / 2, w / 2), style = Stroke(width = sw))
        drawCircle(color, radius = w * 0.05f, center = Offset(w * 0.37f, w * 0.4f))
        drawCircle(color, radius = w * 0.05f, center = Offset(w * 0.63f, w * 0.4f))
        drawArc(color, 20f, 140f, false, topLeft = Offset(w * 0.3f, w * 0.3f), size = Size(w * 0.4f, w * 0.4f), style = Stroke(width = sw, cap = StrokeCap.Round))
    }
}

/** What the send target button shows: a channel (with its platform mark) or "All". */
/** What the box writes to: one chat ([platform]) or every chat ([platforms], the platforms of "All"). Shown as logos. */
data class SendTargetChip(val label: String, val platform: String?, val platforms: List<String> = emptyList())
