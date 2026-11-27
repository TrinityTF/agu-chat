package lol.agu.chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch

/** A thread is opened from a line: a reply names the message it answers and the thread's first one; any other line starts one. */
fun threadOf(vk: String, row: Row, fromReplyLine: Boolean): ThreadReq {
    val r = row.reply
    return if (fromReplyLine && r != null) ThreadReq(vk, row.sk, r.parentId?.content.orEmpty(), r.threadId?.content.orEmpty())
    else ThreadReq(vk, row.sk, row.id?.content.orEmpty(), r?.threadId?.content.orEmpty())
}

/** Whether a line can open a thread: a Twitch or Kick chat line (a whisper has none). */
fun canThread(row: Row) = row.kind == "msg" && (row.platform == "twitch" || row.platform == "kick") && !isWhisperSk(row.sk) && !row.id?.content.isNullOrEmpty()

/**
 * The reply thread over the chat (the desktop's "Reply thread"): the thread's first message and every reply drawn as
 * chat lines, and a box that replies into it. Which lines belong to a thread, and what a reply is sent as, is the core's.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ThreadSheet(vm: ChatViewModel, req: ThreadReq, onDismiss: () -> Unit, onLogin: () -> Unit = {}) {
    var data by remember(req) { mutableStateOf<ThreadData?>(null) }
    val list = vm.rowsFor(req.vk)
    // the thread follows the chat: read again when lines arrive
    LaunchedEffect(req) {
        snapshotFlow { list.size to list.lastOrNull()?.key }.collect { data = vm.threadRows(req) ?: data }
    }
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }
    val focus = remember { FocusRequester() }
    var field by remember(req) { mutableStateOf(TextFieldValue("")) }
    var sending by remember(req) { mutableStateOf(false) }
    var suggestions by remember(req) { mutableStateOf<Suggestions?>(null) }
    var emotes by remember { mutableStateOf(false) }
    fun set(v: TextFieldValue) {
        val t = clampMessage(v.text)
        field = if (t.length == v.text.length) v else v.copy(text = t, selection = TextRange(v.selection.start.coerceAtMost(t.length), v.selection.end.coerceAtMost(t.length)), composition = null)
    }
    val canReply = data?.canReply == true
    LaunchedEffect(field.text, field.selection, canReply) {
        suggestions = if (!canReply || !field.selection.collapsed) null else vm.suggest(req.vk, listOf(req.sk), field.text, field.selection.end)?.takeIf { it.items.isNotEmpty() }
    }
    fun send() {
        val d = data ?: return
        val msg = field.text.trim()
        if (sending || msg.isEmpty() || d.root.isEmpty()) return
        // a command would run as the chat's own: those go in the message box
        val word = msg.split(Regex("\\s+"), limit = 2).first()
        val custom = vm.settings.list("customCommands").any { it.str("name").equals(word, ignoreCase = true) }
        if (msg.startsWith("/") && !custom) { scope.launch { snackbar.showSnackbar("Commands go in the message box below") }; return }
        sending = true
        scope.launch {
            // a reply to the thread's first message, so Twitch keeps it in this thread
            val r = vm.composeSend(req.vk, listOf(req.sk), msg, req.sk to d.root)
            sending = false
            r.toast?.let { snackbar.showSnackbar(it) }
            if (r.clear) set(TextFieldValue(""))
        }
    }

    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheet, containerColor = AguColors.Surface) {
        Column(Modifier.fillMaxHeight(0.92f).imePadding()) {
            val rows = data?.rows.orEmpty()
            val replies = (rows.size - if (data?.hasRoot == true) 1 else 0).coerceAtLeast(0)
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("↩", color = AguColors.Muted, fontSize = 16.sp)
                Text("Reply thread", color = AguColors.Text, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                Text(if (replies == 1) "1 reply" else "$replies replies", color = AguColors.Muted, fontSize = 13.sp, modifier = Modifier.weight(1f))
                Text("×", color = AguColors.Text, fontSize = 24.sp, modifier = Modifier.clip(CircleShape).clickable(onClick = onDismiss).padding(horizontal = 12.dp))
            }
            val state = rememberLazyListState()
            // newest at the bottom, as in the chat
            LaunchedEffect(rows.size) { if (rows.isNotEmpty()) state.scrollToItem(rows.size - 1) }
            CompositionLocalProvider(LocalModMode provides false, LocalThreadOpener provides null) {
                LazyColumn(Modifier.weight(1f).fillMaxWidth().background(AguColors.Bg), state) {
                    if (data != null && data?.hasRoot == false) item("gone") {
                        Text("The message that started this thread is no longer in chat.", color = HintColor, fontSize = 12.sp, modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp))
                    }
                    items(rows, key = { it.key }) { row -> MessageRow(row, onLongPress = {}) }
                }
            }
            if (canReply) Composer(
                value = field, onValue = ::set, canSend = true, locked = false, placeholder = "Reply in thread", showLogin = false, onLogin = onLogin,
                reply = null, onCancelReply = {}, confirm = null, onConfirm = {}, onCancelConfirm = {}, notices = emptyList(),
                suggestions = suggestions,
                onPick = { s ->
                    val sg = suggestions ?: return@Composer
                    val t = field.text
                    val start = sg.start.coerceIn(0, t.length)
                    val end = sg.end.coerceIn(start, t.length)
                    set(TextFieldValue(t.substring(0, start) + s.replace + t.substring(end), TextRange(start + s.replace.length)))
                    suggestions = null
                    runCatching { focus.requestFocus() }
                },
                hasHistory = false, sending = sending, onSend = ::send, onHistory = {}, onEmotes = { emotes = true }, focus = focus,
                modifier = Modifier.navigationBarsPadding(),
            ) else if (data != null) Text("You can't reply in this chat", color = AguColors.Muted, fontSize = 13.sp, modifier = Modifier.fillMaxWidth().padding(16.dp).navigationBarsPadding())
            SnackbarHost(snackbar)
        }
    }
    if (emotes) EmotePickerSheet(vm, listOf(req.sk), field.text, onInsert = { name ->
        val t = field.text
        val start = field.selection.min.coerceIn(0, t.length)
        val end = field.selection.max.coerceIn(start, t.length)
        val lead = if (start == 0 || t[start - 1].isWhitespace()) "" else " "
        val ins = "$lead$name "
        set(TextFieldValue(t.substring(0, start) + ins + t.substring(end), TextRange(start + ins.length)))
    }, onDismiss = { emotes = false; runCatching { focus.requestFocus() } })
}
