package lol.agu.chat.ui

import androidx.compose.foundation.ExperimentalFoundationApi
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
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.foundation.Canvas
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/** The mod controller of the screen (null where nothing moderates), the tab a row is in, and whether mod mode is on there. */
val LocalMod = compositionLocalOf<ModController?> { null }
val LocalModTab = compositionLocalOf<Tab?> { null }
val LocalModMode = compositionLocalOf { false }

/** What a mod dialog is about. */
sealed interface ModDialog {
    /** Twitch: a ban asks for an optional reason. */
    class Ban(val sk: String, val t: ModTarget, val name: String) : ModDialog
    class Warn(val sk: String, val t: ModTarget, val name: String) : ModDialog
    class Timeout(val sk: String, val t: ModTarget, val name: String, val platform: String, val choices: List<Pair<String, Int>>) : ModDialog
    /** Kick, Rumble and YouTube ask before a ban; a command may ask too (`/clear`, `/raid`). */
    class Ask(val text: String, val yes: String, val run: () -> Unit) : ModDialog
}

/** Opens the user card of [user] in [sk]. */
class CardRequest(val sk: String, val platform: String, val user: RowUser)

/**
 * Where moderation runs from (mod buttons, the long-press sheet, the user card, AutoMod): it calls the core, says
 * what happened in a snackbar and owns the dialogs. The rules (which button on which line, who asks first) are the core's.
 */
@Stable
class ModController(private val vm: ChatViewModel, private val scope: CoroutineScope, private val say: (String) -> Unit) {
    var dialog by mutableStateOf<ModDialog?>(null)
    var card by mutableStateOf<CardRequest?>(null)
    /** Counts the actions that worked: an open user card reads the person again. */
    var actionTick by mutableStateOf(0)

    fun toast(text: String?) { if (!text.isNullOrBlank()) say(text) }

    /** Run [action]; a ban on Kick, Rumble or YouTube is asked about first. [done] gets whether it worked. */
    fun act(sk: String, action: String, t: ModTarget, seconds: Int? = null, reason: String = "", quick: Boolean = false, confirmed: Boolean = false, done: (Boolean) -> Unit = {}) {
        scope.launch {
            val r = vm.modAction(sk, action, t, seconds, reason, quick, confirmed)
            if (r.confirm != null) {
                dialog = ModDialog.Ask(r.confirm, "Ban") { act(sk, action, t, seconds, reason, quick, confirmed = true, done) }
                return@launch
            }
            toast(r.toast)
            if (r.ok) actionTick++
            done(r.ok)
        }
    }

    private fun nameOf(t: ModTarget) = t.name.ifEmpty { t.login }.ifEmpty { "this user" }

    fun ban(sk: String, platform: String, t: ModTarget) {
        if (platform == "twitch") dialog = ModDialog.Ban(sk, t, nameOf(t)) else act(sk, "ban", t)
    }

    fun warn(sk: String, t: ModTarget) { dialog = ModDialog.Warn(sk, t, nameOf(t)) }

    /** The lengths to pick from; YouTube picks its own length, so it goes straight through. */
    fun timeout(sk: String, platform: String, m: ModInfo) {
        if (m.choices.isEmpty()) act(sk, "timeout", m.t) else dialog = ModDialog.Timeout(sk, m.t, nameOf(m.t), platform, m.choices)
    }

    fun delete(sk: String, t: ModTarget) = act(sk, "delete", t)

    /** A mod-mode button on [row]: ban and warn open their step, a timeout and a delete go at once (like the desktop's). */
    fun button(vk: String, row: Row, b: ModButtonCfg) {
        val m = row.mod ?: return
        when (b.action) {
            "ban" -> ban(row.sk, row.platform, m.t)
            "timeout" -> act(row.sk, "timeout", m.t, seconds = b.seconds.takeIf { it > 0 }, quick = true)
            "warn" -> warn(row.sk, m.t)
            "delete" -> delete(row.sk, m.t)
            else -> scope.launch {
                val r = vm.modCommand(vk, row.sk, b.command, row)
                toast(r.toast)
                r.confirm?.let { c -> dialog = ModDialog.Ask(c.text, "Confirm") { scope.launch { toast(vm.composeConfirm(c).toast) } } }
            }
        }
    }

    fun automod(sk: String, id: String, allow: Boolean) {
        scope.launch { toast(vm.modAutomod(sk, id, allow).toast) }
    }

    fun openCard(row: Row) {
        val u = row.user ?: return
        if (u.id.isNullOrEmpty() && u.login.isNullOrEmpty() && u.name.isNullOrEmpty()) return
        card = CardRequest(row.sk, row.platform, u)
    }

    suspend fun rules(sk: String) = vm.modRules(sk)
}

// ---- the buttons ---------------------------------------------------------------------------------------

private val BanRed = Color(0xFFF87171)
private val AmberTone = Color(0xFFFBBF24)
private val CmdTone = Color(0xFFA970FF)

internal fun tone(b: ModButtonCfg) = when (b.action) {
    "timeout", "warn" -> AmberTone
    "command" -> CmdTone
    else -> BanRed
}

internal fun allowed(b: ModButtonCfg, m: ModInfo) = when (b.action) {
    "ban" -> m.ban
    "timeout" -> m.timeout
    "warn" -> m.warn
    "delete" -> m.delete
    else -> m.cmd
}

/** The most the mod buttons take of a line's width: more buttons than fit scroll sideways (they never wrap). */
private val StripMax = 156.dp

/**
 * The mod mode buttons at the start of a line, in one compact row like the desktop's: small icons (a label when the
 * button has one) in 36 dp touch targets. Too many for [StripMax] scroll sideways instead of wrapping.
 */
@Composable
fun ModStrip(row: Row, vk: String, ctrl: ModController, modifier: Modifier = Modifier) {
    val m = row.mod ?: return
    val shown = LocalDisplay.current.modButtons.filter { allowed(it, m) }
    if (shown.isEmpty()) return
    Row(modifier.widthIn(max = StripMax).horizontalScroll(rememberScrollState()).padding(start = 2.dp, end = 2.dp), verticalAlignment = Alignment.CenterVertically) {
        for (b in shown) {
            Box(Modifier.size(36.dp).clip(RoundedCornerShape(6.dp)).clickable { ctrl.button(vk, row, b) }, contentAlignment = Alignment.Center) {
                if (b.label.isNotEmpty()) Text(b.label, color = tone(b), fontSize = 10.sp, fontWeight = FontWeight.Bold, maxLines = 1)
                else ModIcon(b.icon.ifEmpty { b.action }, tone(b), 16)
            }
        }
    }
}

/** The mod button icons (the desktop's set), drawn small. */
@Composable
fun ModIcon(name: String, color: Color, sizeDp: Int = 18) {
    Canvas(Modifier.size(sizeDp.dp)) {
        val w = size.width
        val sw = w * 0.1f
        val st = Stroke(width = sw, cap = StrokeCap.Round, join = StrokeJoin.Round)
        fun o(x: Float, y: Float) = Offset(w * x, w * y)
        when (name) {
            "ban" -> {
                drawCircle(color, radius = w * 0.38f, center = o(0.5f, 0.5f), style = st)
                drawLine(color, o(0.24f, 0.76f), o(0.76f, 0.24f), strokeWidth = sw, cap = StrokeCap.Round)
            }
            "clock", "timeout" -> {
                drawCircle(color, radius = w * 0.38f, center = o(0.5f, 0.5f), style = st)
                drawLine(color, o(0.5f, 0.5f), o(0.5f, 0.26f), strokeWidth = sw, cap = StrokeCap.Round)
                drawLine(color, o(0.5f, 0.5f), o(0.68f, 0.6f), strokeWidth = sw, cap = StrokeCap.Round)
            }
            "trash", "delete" -> {
                drawLine(color, o(0.18f, 0.27f), o(0.82f, 0.27f), strokeWidth = sw, cap = StrokeCap.Round)
                drawLine(color, o(0.4f, 0.27f), o(0.4f, 0.16f), strokeWidth = sw, cap = StrokeCap.Round)
                drawLine(color, o(0.6f, 0.27f), o(0.6f, 0.16f), strokeWidth = sw, cap = StrokeCap.Round)
                val p = Path().apply { moveTo(w * 0.26f, w * 0.27f); lineTo(w * 0.32f, w * 0.84f); lineTo(w * 0.68f, w * 0.84f); lineTo(w * 0.74f, w * 0.27f) }
                drawPath(p, color, style = st)
            }
            "shield" -> {
                val p = Path().apply {
                    moveTo(w * 0.5f, w * 0.12f); lineTo(w * 0.82f, w * 0.24f); lineTo(w * 0.8f, w * 0.52f)
                    cubicTo(w * 0.78f, w * 0.7f, w * 0.64f, w * 0.82f, w * 0.5f, w * 0.9f)
                    cubicTo(w * 0.36f, w * 0.82f, w * 0.22f, w * 0.7f, w * 0.2f, w * 0.52f); lineTo(w * 0.18f, w * 0.24f); close()
                }
                drawPath(p, color, style = st)
            }
            "warning", "warn" -> {
                val p = Path().apply { moveTo(w * 0.5f, w * 0.14f); lineTo(w * 0.88f, w * 0.8f); lineTo(w * 0.12f, w * 0.8f); close() }
                drawPath(p, color, style = st)
                drawLine(color, o(0.5f, 0.4f), o(0.5f, 0.58f), strokeWidth = sw, cap = StrokeCap.Round)
                drawCircle(color, radius = sw * 0.6f, center = o(0.5f, 0.69f))
            }
            "flag" -> {
                drawLine(color, o(0.24f, 0.14f), o(0.24f, 0.88f), strokeWidth = sw, cap = StrokeCap.Round)
                val p = Path().apply { moveTo(w * 0.24f, w * 0.2f); lineTo(w * 0.8f, w * 0.2f); lineTo(w * 0.66f, w * 0.38f); lineTo(w * 0.8f, w * 0.56f); lineTo(w * 0.24f, w * 0.56f) }
                drawPath(p, color, style = st)
            }
            "eyeoff" -> {
                drawOval(color, topLeft = o(0.1f, 0.3f), size = Size(w * 0.8f, w * 0.4f), style = st)
                drawCircle(color, radius = w * 0.09f, center = o(0.5f, 0.5f))
                drawLine(color, o(0.2f, 0.82f), o(0.8f, 0.18f), strokeWidth = sw, cap = StrokeCap.Round)
            }
            "pin" -> {
                drawCircle(color, radius = w * 0.2f, center = o(0.5f, 0.32f), style = st)
                drawLine(color, o(0.5f, 0.52f), o(0.5f, 0.88f), strokeWidth = sw, cap = StrokeCap.Round)
            }
            "check" -> {
                val p = Path().apply { moveTo(w * 0.2f, w * 0.52f); lineTo(w * 0.42f, w * 0.72f); lineTo(w * 0.8f, w * 0.28f) }
                drawPath(p, color, style = st)
            }
            else -> {
                // chat: a bubble with a tail
                drawRoundRect(color, topLeft = o(0.12f, 0.16f), size = Size(w * 0.76f, w * 0.54f), cornerRadius = androidx.compose.ui.geometry.CornerRadius(w * 0.12f), style = st)
                val p = Path().apply { moveTo(w * 0.3f, w * 0.7f); lineTo(w * 0.28f, w * 0.88f); lineTo(w * 0.48f, w * 0.7f) }
                drawPath(p, color, style = st)
            }
        }
    }
}

// ---- dialogs -------------------------------------------------------------------------------------------

@Composable
internal fun FieldBox(value: String, onChange: (String) -> Unit, placeholder: String, modifier: Modifier = Modifier, singleLine: Boolean = true) {
    Box(modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(AguColors.Surface2).padding(horizontal = 14.dp, vertical = 12.dp)) {
        if (value.isEmpty()) Text(placeholder, color = AguColors.Muted, fontSize = 15.sp)
        BasicTextField(value, onChange, singleLine = singleLine, textStyle = TextStyle(color = AguColors.Text, fontSize = 15.sp), cursorBrush = SolidColor(AguColors.Accent), modifier = Modifier.fillMaxWidth())
    }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun ModDialogs(ctrl: ModController) {
    when (val d = ctrl.dialog) {
        null -> {}
        is ModDialog.Ban -> {
            var reason by remember(d) { mutableStateOf("") }
            AlertDialog(
                onDismissRequest = { ctrl.dialog = null },
                title = { Text("Ban ${d.name}?") },
                text = { FieldBox(reason, { reason = it.take(500) }, "Reason (optional)") },
                confirmButton = { TextButton(onClick = { ctrl.dialog = null; ctrl.act(d.sk, "ban", d.t, reason = reason.trim()) }) { Text("Confirm", color = AguColors.Danger) } },
                dismissButton = { TextButton(onClick = { ctrl.dialog = null }) { Text("Cancel", color = AguColors.Muted) } },
            )
        }
        is ModDialog.Warn -> WarnDialog(ctrl, d)
        is ModDialog.Ask -> AlertDialog(
            onDismissRequest = { ctrl.dialog = null },
            title = { Text(d.text) },
            confirmButton = { TextButton(onClick = { ctrl.dialog = null; d.run() }) { Text(d.yes, color = AguColors.Danger) } },
            dismissButton = { TextButton(onClick = { ctrl.dialog = null }) { Text("Cancel", color = AguColors.Muted) } },
        )
        is ModDialog.Timeout -> ModalBottomSheet(onDismissRequest = { ctrl.dialog = null }, containerColor = AguColors.Surface) {
            Column(Modifier.navigationBarsPadding().padding(start = 20.dp, end = 20.dp, bottom = 16.dp)) {
                Text("Timeout ${d.name}", color = AguColors.Text, fontSize = 16.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(vertical = 8.dp))
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    for ((label, secs) in d.choices) {
                        Text(
                            label, color = AguColors.Text, fontSize = 15.sp,
                            modifier = Modifier.clip(RoundedCornerShape(8.dp)).border(1.dp, AguColors.Surface2, RoundedCornerShape(8.dp))
                                .clickable { ctrl.dialog = null; ctrl.act(d.sk, "timeout", d.t, seconds = secs) }.padding(horizontal = 16.dp, vertical = 10.dp),
                        )
                    }
                }
            }
        }
    }
}

/** A warning: tick the chat rules that were broken and/or write a reason (at least one). */
@Composable
private fun WarnDialog(ctrl: ModController, d: ModDialog.Warn) {
    var rules by remember(d) { mutableStateOf<List<String>>(emptyList()) }
    val picked = remember(d) { mutableStateListOf<String>() }
    var custom by remember(d) { mutableStateOf("") }
    LaunchedEffect(d) { rules = ctrl.rules(d.sk) }
    val reason = (picked.toList() + custom.trim()).filter { it.isNotEmpty() }.joinToString("; ").take(500)
    AlertDialog(
        onDismissRequest = { ctrl.dialog = null },
        title = { Text("Warn ${d.name}") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                for (r in rules) Row(Modifier.fillMaxWidth().clickable { if (!picked.remove(r)) picked.add(r) }, verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    val on = r in picked
                    Box(Modifier.padding(top = 2.dp).size(18.dp).clip(RoundedCornerShape(4.dp)).border(1.5.dp, if (on) AguColors.Accent else AguColors.Muted, RoundedCornerShape(4.dp)).background(if (on) AguColors.Accent else Color.Transparent), contentAlignment = Alignment.Center) {
                        if (on) ModIcon("check", Color.White, 14)
                    }
                    Text(r, color = AguColors.Text, fontSize = 14.sp, modifier = Modifier.weight(1f))
                }
                FieldBox(custom, { custom = it.take(500) }, "Custom reason")
            }
        },
        confirmButton = { TextButton(enabled = reason.isNotEmpty(), onClick = { ctrl.dialog = null; ctrl.act(d.sk, "warn", d.t, reason = reason) }) { Text("Warn", color = if (reason.isNotEmpty()) AguColors.Danger else AguColors.Muted) } },
        dismissButton = { TextButton(onClick = { ctrl.dialog = null }) { Text("Cancel", color = AguColors.Muted) } },
    )
}

// ---- the long-press sheet --------------------------------------------------------------------------

/** What a long-press on a line offers: Reply, Copy, the user card, and for a moderator what the core allows on that line. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RowActionsSheet(ctrl: ModController?, row: Row, onReply: (() -> Unit)?, onCopy: () -> Unit, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = AguColors.Surface) {
        Column(Modifier.navigationBarsPadding().verticalScroll(rememberScrollState()).padding(bottom = 12.dp)) {
            Text(
                "${row.user?.name ?: row.user?.login ?: ""}: ${row.plainText()}".take(120),
                color = AguColors.Muted, fontSize = 13.sp, maxLines = 2, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp),
            )
            if (onReply != null) SheetRow("Reply", onClick = onReply)
            SheetRow("Copy text", onClick = onCopy)
            if (ctrl != null && row.user != null && row.kind != "modfeed") SheetRow("Open user card") { onDismiss(); ctrl.openCard(row) }
            val m = row.mod
            if (ctrl != null && m != null) {
                if (m.timeout) SheetRow("Timeout…") { onDismiss(); ctrl.timeout(row.sk, row.platform, m) }
                if (m.delete) SheetRow("Delete message", AguColors.Danger) { onDismiss(); ctrl.delete(row.sk, m.t) }
                if (m.warn) SheetRow("Warn…") { onDismiss(); ctrl.warn(row.sk, m.t) }
                if (m.ban) SheetRow("Ban…", AguColors.Danger) { onDismiss(); ctrl.ban(row.sk, row.platform, m.t) }
            }
        }
    }
}

@Composable
internal fun SheetRow(label: String, color: Color = AguColors.Text, onClick: () -> Unit) {
    Text(label, color = color, fontSize = 16.sp, modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 20.dp, vertical = 14.dp))
}

/** A small pill button (Allow / Deny, Load more...). */
@Composable
internal fun Pill(label: String, color: Color, modifier: Modifier = Modifier, enabled: Boolean = true, onClick: () -> Unit) {
    Text(
        label, color = if (enabled) color else AguColors.Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
        modifier = modifier.clip(CircleShape).border(1.dp, (if (enabled) color else AguColors.Muted).copy(alpha = 0.5f), CircleShape).clickable(enabled = enabled, onClick = onClick).padding(horizontal = 14.dp, vertical = 7.dp),
    )
}
