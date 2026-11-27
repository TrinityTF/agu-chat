package lol.agu.chat.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.foundation.layout.navigationBarsPadding
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * The message box of a tab with everything around it: the suggestion strip, the emote picker, the history, the send
 * target, the reply, the confirm for `/clear` and `/raid`, and the chat modes that lock the box. What goes out, and
 * how a command is run, is the core's (the desktop's rules); this only asks and shows the answer.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ComposerHost(
    vm: ChatViewModel,
    tab: Tab,
    replyTo: Row?,
    onClearReply: () -> Unit,
    /** a name to put at the start of the message (a Super Chat's Reply), once */
    mention: String?,
    onMentionUsed: () -> Unit,
    snackbar: SnackbarHostState,
    onLogin: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    val focus = remember { FocusRequester() }
    val history = vm.historyOf(tab.id)

    var field by remember(tab.id) { mutableStateOf(vm.drafts[tab.id].orEmpty().let { TextFieldValue(it, TextRange(it.length)) }) }
    fun set(v: TextFieldValue) {
        val t = clampMessage(v.text)
        field = if (t.length == v.text.length) v else v.copy(text = t, selection = TextRange(v.selection.start.coerceAtMost(t.length), v.selection.end.coerceAtMost(t.length)), composition = null)
        vm.drafts[tab.id] = field.text
    }
    fun setText(t: String, caret: Int = t.length) = set(TextFieldValue(t, TextRange(caret.coerceIn(0, t.length))))

    var sending by remember { mutableStateOf(false) }
    var confirm by remember { mutableStateOf<Confirm?>(null) }
    var picking by remember { mutableStateOf(false) }
    var emotes by remember { mutableStateOf(false) }
    var recent by remember { mutableStateOf(false) }
    var suggestions by remember { mutableStateOf<Suggestions?>(null) }
    var gates by remember { mutableStateOf<Map<String, Gate>>(emptyMap()) }

    val options = sendOptions(tab, vm.conns)
    val merged = options.size > 1
    val sendable = options.filter { it.canSend }
    val connNow = tab.sks.map { vm.conns[it] }
    // how the chats' modes apply to you: when a chat connects, when you log in, and every half minute after
    LaunchedEffect(tab.id, tab.sks, connNow, vm.accounts) {
        while (true) {
            gates = vm.gates(tab.id, tab.sks)
            delay(30_000)
        }
    }
    // a chat whose mode keeps you out is not a target of "all chats"
    val broadcast = sendable.filter { gates[it.sk]?.blocked != true }.map { it.sk }
    val chosen = chosenTarget(vm.targets[tab.id], sendable.map { it.sk }, replyTo?.sk, broadcast)
    val targets = if (chosen == ALL) broadcast else listOf(chosen).filter { it.isNotEmpty() }
    val locked = chosen != ALL && gates[chosen]?.blocked == true
    val canSend = sendable.isNotEmpty()
    val first = options.firstOrNull()
    val chosenOpt = options.firstOrNull { it.sk == chosen }
    val replyName = replyTo?.let { it.user?.name ?: it.user?.login }

    // the strip follows the text and the caret (the core's rules: a mention, a command, ":emote", a word's start)
    LaunchedEffect(field.text, field.selection, chosen, targets, canSend, locked) {
        suggestions = if (!canSend || locked || targets.isEmpty() || !field.selection.collapsed) null
        else vm.suggest(tab.id, targets, field.text, field.selection.end)?.takeIf { it.items.isNotEmpty() }
    }
    // Reply puts the caret in the box
    LaunchedEffect(replyTo?.key) { if (replyTo != null) runCatching { focus.requestFocus() } }
    LaunchedEffect(mention) {
        if (mention != null) {
            setText("@$mention " + field.text)
            onMentionUsed()
            runCatching { focus.requestFocus() }
        }
    }

    fun toast(t: String) { scope.launch { snackbar.showSnackbar(t) } }

    fun done(r: SendResult, sent: String?) {
        r.toast?.let { toast(it) }
        r.confirm?.let { confirm = it }
        if (r.clear) {
            if (sent != null) history.add(sent)
            setText("")
            suggestions = null
            onClearReply()
        }
    }

    fun send() {
        if (sending || locked) return
        val msg = field.text.trim()
        if (msg.isEmpty() || targets.isEmpty()) return
        sending = true
        scope.launch {
            val r = vm.composeSend(tab.id, targets, msg, replyTo?.id?.content?.let { replyTo.sk to it })
            sending = false
            done(r, msg)
        }
    }

    fun pick(s: Suggestion) {
        val sg = suggestions ?: return
        val t = field.text
        val start = sg.start.coerceIn(0, t.length)
        val end = sg.end.coerceIn(start, t.length)
        setText(t.substring(0, start) + s.replace + t.substring(end), start + s.replace.length)
        suggestions = null
        runCatching { focus.requestFocus() }
    }

    fun insertEmote(name: String) {
        val t = field.text
        val start = field.selection.min.coerceIn(0, t.length)
        val end = field.selection.max.coerceIn(start, t.length)
        val lead = if (start == 0 || t[start - 1].isWhitespace()) "" else " "
        val ins = "$lead$name "
        setText(t.substring(0, start) + ins + t.substring(end), start + ins.length)
    }

    val notices: List<Gate> = when {
        chosen == ALL -> sendable.filter { gates[it.sk] != null }.map { o -> gates.getValue(o.sk).let { g -> g.copy(text = "${platformLabel(o.platform)}: ${g.text}") } }
        locked -> emptyList()
        else -> listOfNotNull(gates[chosen])
    }
    val placeholder = when {
        !canSend -> first?.let { readOnlyText(vm.conns[it.sk], it.platform) } ?: "Chat is read-only"
        locked -> gates[chosen]?.text.orEmpty()
        replyName != null -> "Reply to @$replyName"
        chosen == ALL -> "Send to " + broadcast.joinToString(" + ") { platformLabel(it.substringBefore(':')) }
        chosenOpt?.whisper == true -> "Whisper ${chosenOpt.channel}"
        else -> "Message " + (if (chosenOpt?.platform == "twitch") "#" else "") + chosenOpt?.channel.orEmpty()
    }

    Composer(
        value = field,
        onValue = ::set,
        canSend = canSend,
        locked = locked,
        placeholder = placeholder,
        showLogin = !canSend && options.any { vm.conns[it.sk]?.loggedOut == true },
        onLogin = onLogin,
        reply = replyTo?.let { ReplyBar(replyName.orEmpty(), it.plainText().take(120)) },
        onCancelReply = onClearReply,
        confirm = confirm,
        onConfirm = {
            val c = confirm ?: return@Composer
            sending = true
            scope.launch {
                val r = vm.composeConfirm(c)
                sending = false
                confirm = null
                done(r, null)
            }
        },
        onCancelConfirm = { confirm = null },
        notices = notices,
        suggestions = suggestions,
        onPick = ::pick,
        hasHistory = history.list.isNotEmpty(),
        sending = sending,
        onSend = ::send,
        onHistory = { recent = true },
        onEmotes = { emotes = true },
        focus = focus,
        modifier = modifier,
        target = if (merged && canSend) {
            if (chosen == ALL) SendTargetChip("All", null, broadcast.map { it.substringBefore(':') }) else chosenOpt?.let { SendTargetChip(it.channel, it.platform) }
        } else null,
        onTarget = { picking = true },
    )

    if (emotes) EmotePickerSheet(vm, targets, field.text, onInsert = ::insertEmote, onDismiss = { emotes = false; runCatching { focus.requestFocus() } })
    if (recent) HistorySheet(history.list, onPick = { setText(it); recent = false; runCatching { focus.requestFocus() } }, onDismiss = { recent = false })
    if (picking) {
        ModalBottomSheet(onDismissRequest = { picking = false }, containerColor = AguColors.Surface) {
            Column(Modifier.navigationBarsPadding().padding(bottom = 12.dp)) {
                Text("Send to", color = AguColors.Muted, fontSize = 13.sp, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
                for (o in options) {
                    val lock = gates[o.sk]?.blocked == true
                    TargetRow(o.channel, o.platform, o.reason ?: if (lock) "locked" else null, selected = chosen == o.sk, enabled = o.canSend) {
                        vm.targets[tab.id] = o.sk; picking = false
                    }
                }
                if (broadcast.size > 1) TargetRow("All chats", null, null, selected = chosen == ALL, enabled = true) {
                    vm.targets[tab.id] = ALL; picking = false
                }
            }
        }
    }
}
