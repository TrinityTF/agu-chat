package lol.agu.chat.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import lol.agu.chat.R

@Composable
fun MainScreen(vm: ChatViewModel) {
    var showAccounts by rememberSaveable { mutableStateOf(false) }
    Box(Modifier.fillMaxSize().background(AguColors.Bg)) {
        Chats(vm, onAccounts = { showAccounts = true })
        if (vm.mentionsOpen) MentionsScreen(vm)
        if (showAccounts) AccountsScreen(vm, onBack = { showAccounts = false })
        vm.loginRequest?.let { LoginWebScreen(it, vm) }
    }
}

/** Where the add-channel dialog puts the channel: a new tab, or merged into [tab]. */
private class AddTo(val tab: Tab?)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Chats(vm: ChatViewModel, onAccounts: () -> Unit) {
    val tabs = vm.tabs
    val pager = rememberPagerState { tabs.size }
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }
    val ctx = LocalContext.current

    var adding by remember { mutableStateOf<AddTo?>(null) }
    var tabMenu by remember { mutableStateOf<Tab?>(null) }
    var renaming by remember { mutableStateOf<Tab?>(null) }
    var picking by remember { mutableStateOf(false) }
    var menuRow by remember { mutableStateOf<Row?>(null) }
    var replyTo by remember { mutableStateOf<Row?>(null) }
    val drafts = remember { HashMap<String, String>() }
    val targets = remember { HashMap<String, String>() } // tab id -> "*" (all chats) or a source key
    var text by remember { mutableStateOf("") }

    val current = tabs.getOrNull(pager.currentPage)

    // Jump to a newly added tab (but not when the saved tabs are first restored).
    var prevCount by remember { mutableStateOf(tabs.size) }
    LaunchedEffect(tabs.size) {
        if (tabs.size > prevCount && prevCount > 0) pager.animateScrollToPage(tabs.size - 1)
        prevCount = tabs.size
    }
    // Each tab keeps its own draft; replying is per tab too. The tab you look at has seen its messages.
    LaunchedEffect(current?.id) { replyTo = null; text = drafts[current?.id].orEmpty(); vm.select(current?.id) }
    // A tapped mention: its tab comes forward (the list scrolls to the message itself)
    val jump = vm.jump
    LaunchedEffect(jump) {
        val i = tabs.indexOfFirst { it.id == jump?.vk }
        if (i >= 0 && i != pager.currentPage) pager.animateScrollToPage(i)
    }

    val options = current?.let { sendOptions(it, vm.conns) }.orEmpty()
    val merged = options.size > 1
    val sendable = options.filter { it.canSend }
    val chosen = chosenTarget(targets[current?.id], sendable.map { it.sk }, replyTo?.sk)

    Column(Modifier.fillMaxSize().statusBarsPadding().imePadding()) {
        TopBar(onAccounts, vm.mentionsUnread, onBell = { vm.openMentions() })
        TabStrip(
            tabs = tabs, selected = pager.currentPage, vm = vm,
            onSelect = { i -> scope.launch { pager.animateScrollToPage(i) } },
            onMenu = { tabMenu = it },
            onAdd = { adding = AddTo(null) },
        )
        if (tabs.isEmpty()) {
            Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    Text("Add a channel to start", color = AguColors.Muted, fontSize = 16.sp)
                    Box(
                        Modifier.size(56.dp).clip(CircleShape).background(AguColors.Brand).clickable { adding = AddTo(null) },
                        contentAlignment = Alignment.Center,
                    ) { Text("+", color = Color.White, fontSize = 30.sp) }
                }
            }
        } else {
            HorizontalPager(pager, Modifier.weight(1f), key = { tabs.getOrNull(it)?.id ?: it }) { page ->
                val tab = tabs.getOrNull(page)
                if (tab != null) ChatList(vm, tab, jump?.takeIf { it.vk == tab.id }, onLongPress = { menuRow = it })
            }
            val single = current?.sks?.firstOrNull()?.let { vm.conns[it] }
            val first = options.firstOrNull()
            val noneSendable = merged && sendable.isEmpty()
            Composer(
                text = text,
                onText = { text = it; current?.let { t -> drafts[t.id] = it } },
                canSend = if (merged) !noneSendable else single?.canSend != false,
                readOnlyReason = if (merged) first?.let { readOnlyText(vm.conns[it.sk], it.platform) } else readOnlyText(single, current?.channels?.firstOrNull()?.platform),
                showLogin = if (merged) options.any { vm.conns[it.sk]?.loggedOut == true } && noneSendable else single?.loggedOut == true,
                replyingTo = replyTo?.let { it.user?.name ?: it.user?.login },
                onCancelReply = { replyTo = null },
                onSend = {
                    val tab = current ?: return@Composer
                    val msg = text.trim()
                    if (msg.isEmpty()) return@Composer
                    val sks = if (chosen == ALL) sendable.map { it.sk } else listOf(chosen.ifEmpty { options.firstOrNull()?.sk ?: return@Composer })
                    val reply = replyTo
                    scope.launch {
                        val failed = ArrayList<String>()
                        for (sk in sks) {
                            val err = vm.send(sk, msg, reply?.id?.content?.takeIf { reply.sk == sk })
                            if (err != null) failed.add(if (merged) "${platformLabel(sk.substringBefore(':'))}: $err" else err)
                        }
                        if (failed.size < sks.size) { text = ""; drafts[tab.id] = ""; replyTo = null }
                        if (failed.isNotEmpty()) snackbar.showSnackbar(failed.joinToString("\n"))
                    }
                },
                onLogin = onAccounts,
                modifier = Modifier.navigationBarsPadding(),
                target = if (merged && !noneSendable) {
                    if (chosen == ALL) SendTargetChip("All", null)
                    else options.firstOrNull { it.sk == chosen }?.let { SendTargetChip(it.channel, it.platform) }
                } else null,
                onTarget = { picking = true },
                placeholder = if (merged) (if (chosen == ALL) "Message all chats" else "Message ${options.firstOrNull { it.sk == chosen }?.channel.orEmpty()}") else "Send a message",
            )
        }
        SnackbarHost(snackbar)
    }

    adding?.let { a ->
        AddChannelDialog(vm, into = a.tab, mergeInto = current, onDismiss = { adding = null })
    }

    tabMenu?.let { tab ->
        ModalBottomSheet(onDismissRequest = { tabMenu = null }, containerColor = AguColors.Surface) {
            Column(Modifier.navigationBarsPadding().padding(bottom = 12.dp)) {
                Text(tab.title(full = true), color = AguColors.Muted, fontSize = 13.sp, maxLines = 1, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
                SheetItem("Rename") { renaming = tab; tabMenu = null }
                SheetItem("Add a channel to this tab") { adding = AddTo(tab); tabMenu = null }
                if (tab.channels.size > 1) for (ch in tab.channels) {
                    SheetItem("Remove ${platformLabel(ch.platform)} ${ch.channel}") { vm.removeChannel(tab, ch); tabMenu = null }
                }
                SheetItem("Close tab", color = AguColors.Danger) { vm.closeTab(tab); tabMenu = null }
            }
        }
    }

    renaming?.let { tab ->
        var name by remember { mutableStateOf(tab.name) }
        AlertDialog(
            onDismissRequest = { renaming = null },
            title = { Text("Rename tab") },
            text = {
                Box(Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(AguColors.Surface2).padding(horizontal = 14.dp, vertical = 12.dp)) {
                    if (name.isEmpty()) Text(tab.title(full = true), color = AguColors.Muted, fontSize = 15.sp)
                    BasicTextField(
                        name, { name = it }, singleLine = true,
                        textStyle = TextStyle(color = AguColors.Text, fontSize = 15.sp),
                        cursorBrush = SolidColor(AguColors.Accent), modifier = Modifier.fillMaxWidth(),
                    )
                }
            },
            confirmButton = { TextButton(onClick = { vm.renameTab(tab, name); renaming = null }) { Text("Save", color = AguColors.Accent) } },
            dismissButton = { TextButton(onClick = { renaming = null }) { Text("Cancel", color = AguColors.Muted) } },
        )
    }

    if (picking && current != null) {
        ModalBottomSheet(onDismissRequest = { picking = false }, containerColor = AguColors.Surface) {
            Column(Modifier.navigationBarsPadding().padding(bottom = 12.dp)) {
                Text("Send to", color = AguColors.Muted, fontSize = 13.sp, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
                for (o in options) {
                    TargetRow(o.channel, o.platform, o.reason, selected = chosen == o.sk, enabled = o.canSend) {
                        targets[current.id] = o.sk; picking = false
                    }
                }
                if (sendable.size > 1) TargetRow("All chats", null, null, selected = chosen == ALL, enabled = true) {
                    targets[current.id] = ALL; picking = false
                }
            }
        }
    }

    menuRow?.let { row ->
        ModalBottomSheet(onDismissRequest = { menuRow = null }, containerColor = AguColors.Surface) {
            Column(Modifier.navigationBarsPadding().padding(bottom = 12.dp)) {
                Text(
                    "${row.user?.name ?: row.user?.login ?: ""}: ${row.plainText()}".take(120),
                    color = AguColors.Muted, fontSize = 13.sp, maxLines = 2, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp),
                )
                SheetItem("Reply") { replyTo = row; menuRow = null }
                SheetItem("Copy text") { copyText(ctx, row.plainText()); menuRow = null }
            }
        }
    }
}

@Composable
private fun SheetItem(label: String, color: Color = AguColors.Text, onClick: () -> Unit) {
    Text(label, color = color, fontSize = 16.sp, modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 20.dp, vertical = 14.dp))
}

@Composable
private fun TargetRow(label: String, platform: String?, reason: String?, selected: Boolean, enabled: Boolean, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().clickable(enabled = enabled, onClick = onClick).padding(horizontal = 20.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(Modifier.size(18.dp)) {
            if (platform != null) PlatformMark(platform) else Text("All", color = AguColors.Muted, fontSize = 10.sp, fontWeight = FontWeight.Bold)
        }
        Text(label, color = if (enabled) AguColors.Text else AguColors.Muted, fontSize = 16.sp, modifier = Modifier.weight(1f), maxLines = 1)
        if (reason != null) Text(reason, color = AguColors.Muted, fontSize = 12.sp)
        else if (selected) Text("✓", color = AguColors.Accent, fontSize = 16.sp)
    }
}

private fun copyText(ctx: Context, text: String) {
    (ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("message", text))
}

@Composable
private fun TopBar(onAccounts: () -> Unit, mentions: Int, onBell: () -> Unit) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(30.dp).clip(RoundedCornerShape(9.dp)).background(AguColors.Brand), contentAlignment = Alignment.Center) {
            // the launcher glyph (40 of its 108 units): drawn at 45dp its bubbles fill ~55% of the square, as on agu.lol/app
            Image(painterResource(R.drawable.ic_launcher_foreground), contentDescription = null, modifier = Modifier.requiredSize(45.dp))
        }
        Text("Agu Chat", color = AguColors.Text, fontSize = 19.sp, fontWeight = FontWeight.Bold, modifier = Modifier.padding(start = 10.dp).weight(1f))
        Box(Modifier.size(40.dp).clip(CircleShape).clickable(onClick = onBell), contentAlignment = Alignment.Center) {
            val red = mentions > 0
            val c = if (red) MentionBell else AguColors.Text
            Canvas(Modifier.size(22.dp)) {
                val w = size.width
                val h = size.height
                val body = Path().apply {
                    moveTo(w * 0.5f, h * 0.08f)
                    cubicTo(w * 0.22f, h * 0.08f, w * 0.2f, h * 0.34f, w * 0.2f, h * 0.5f)
                    lineTo(w * 0.1f, h * 0.76f)
                    lineTo(w * 0.9f, h * 0.76f)
                    lineTo(w * 0.8f, h * 0.5f)
                    cubicTo(w * 0.8f, h * 0.34f, w * 0.78f, h * 0.08f, w * 0.5f, h * 0.08f)
                    close()
                }
                drawPath(body, c)
                drawCircle(c, radius = w * 0.11f, center = Offset(w * 0.5f, h * 0.88f))
            }
            if (red) {
                Box(
                    Modifier.align(Alignment.TopEnd).offset(x = 2.dp, y = (-1).dp).height(17.dp).clip(RoundedCornerShape(9.dp)).background(MentionBell)
                        .padding(horizontal = 5.dp),
                    contentAlignment = Alignment.Center,
                ) { Text(if (mentions > 99) "99+" else mentions.toString(), color = Color.White, fontSize = 10.sp, fontWeight = FontWeight.Bold, lineHeight = 10.sp) }
            }
        }
        Box(Modifier.size(40.dp).clip(CircleShape).clickable(onClick = onAccounts), contentAlignment = Alignment.Center) {
            // A simple person glyph: head and shoulders.
            Canvas(Modifier.size(22.dp)) {
                val c = AguColors.Text
                drawCircle(c, radius = size.width * 0.22f, center = Offset(size.width / 2, size.height * 0.30f))
                drawArc(c, 180f, 180f, true, topLeft = Offset(size.width * 0.12f, size.height * 0.58f), size = Size(size.width * 0.76f, size.height * 0.8f))
            }
        }
    }
}

private val MentionBell = Color(0xFFEF4444)
private val LiveGreen = Color(0xFF22C55E)

/** The dot of a tab: red for a mention or highlight, green while a channel is live, grey for new messages. The tab you are on has seen its messages, so only live shows. */
private fun tabDot(selected: Boolean, unread: Dot?, live: Boolean): Dot? = when {
    selected -> if (live) Dot.Live else null
    unread == Dot.Highlight -> Dot.Highlight
    live -> Dot.Live
    unread == Dot.Message -> Dot.Message
    else -> null
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun TabStrip(tabs: List<Tab>, selected: Int, vm: ChatViewModel, onSelect: (Int) -> Unit, onMenu: (Tab) -> Unit, onAdd: () -> Unit) {
    Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 6.dp)) {
        tabs.forEachIndexed { i, tab ->
            val sel = i == selected
            val live = tab.sks.any { vm.conns[it]?.live == true }
            val dot = tabDot(sel, vm.unread[tab.id], live)
            Column(
                Modifier.combinedClickable(onClick = { onSelect(i) }, onLongClick = { onMenu(tab) }).padding(horizontal = 6.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Row(
                    Modifier.height(42.dp).drawBehind {
                        if (sel) drawRect(AguColors.Accent, Offset(0f, size.height - 2.dp.toPx()), Size(size.width, 2.dp.toPx()))
                    }.padding(horizontal = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (dot != null) {
                        Box(
                            Modifier.padding(end = 6.dp).size(8.dp).clip(CircleShape).background(
                                when (dot) { Dot.Highlight -> MentionBell; Dot.Live -> LiveGreen; Dot.Message -> AguColors.Muted },
                            ),
                        )
                    }
                    for (ch in tab.channels.take(3)) Box(Modifier.padding(end = 3.dp).size(7.dp).clip(CircleShape).background(platformColor(ch.platform)))
                    Text(
                        tab.title(), color = if (sel) AguColors.Text else AguColors.Muted, fontSize = 14.sp,
                        fontWeight = if (sel) FontWeight.SemiBold else FontWeight.Normal, modifier = Modifier.padding(start = 4.dp),
                    )
                }
            }
        }
        Box(Modifier.height(42.dp).size(42.dp).clip(CircleShape).clickable(onClick = onAdd), contentAlignment = Alignment.Center) {
            Text("+", color = AguColors.Text, fontSize = 22.sp)
        }
    }
}

@Composable
private fun ChatList(vm: ChatViewModel, tab: Tab, jump: Jump?, onLongPress: (Row) -> Unit) {
    val rows = vm.rowsFor(tab.id)
    val state = rememberLazyListState()
    val scope = rememberCoroutineScope()
    // "At the bottom" is judged from the previous layout, so a freshly appended row does not unstick us.
    val atBottom by remember { derivedStateOf { !state.canScrollForward } }
    var flashKey by remember { mutableStateOf<String?>(null) }
    val merged = tab.channels.size > 1

    LaunchedEffect(rows.size) {
        if (atBottom && rows.isNotEmpty()) state.scrollToItem(rows.lastIndex)
    }
    // A tapped mention: scroll to its message and mark it for a moment
    LaunchedEffect(jump) {
        if (jump == null) return@LaunchedEffect
        val i = rows.indexOfFirst { it.key == jump.key }
        if (i >= 0) {
            state.scrollToItem((i - 2).coerceAtLeast(0))
            flashKey = jump.key
            vm.jumpDone(jump)
            delay(1800)
            flashKey = null
        } else vm.jumpDone(jump)
    }

    Box(Modifier.fillMaxSize()) {
        LazyColumn(Modifier.fillMaxSize(), state) {
            items(rows, key = { it.key }) { row -> MessageRow(row, onLongPress, showMark = merged, flash = row.key == flashKey) }
        }
        // channels that are not connected say so (named by platform when several are merged)
        val lines = tab.channels.mapNotNull { ch ->
            val conn = vm.conns[ch.sk()] ?: return@mapNotNull null
            if (conn.state == "open") return@mapNotNull null
            val label = when (conn.state) {
                "connecting" -> "Connecting…"
                "offline" -> "Offline"
                "error" -> "Connection error"
                "unavailable" -> "Unavailable"
                else -> conn.state
            }
            Triple(if (merged) "${platformLabel(ch.platform)}: $label" else label, conn.detail, ch.sk())
        }
        if (lines.isNotEmpty()) {
            Column(Modifier.fillMaxWidth().background(AguColors.Surface.copy(alpha = 0.95f)).padding(horizontal = 14.dp, vertical = 6.dp)) {
                for ((label, detail, _) in lines) {
                    Text(label, color = AguColors.Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    if (!detail.isNullOrBlank()) Text(detail, color = AguColors.Muted, fontSize = 12.sp)
                }
            }
        }
        if (!atBottom && rows.isNotEmpty()) {
            Text(
                "New messages ↓", color = Color.White, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 12.dp)
                    .clip(RoundedCornerShape(50)).background(AguColors.Accent)
                    .clickable { scope.launch { state.scrollToItem(rows.lastIndex) } }
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
    }
}

/** Add a channel: to a new tab, or merged into the tab you are on ([mergeInto]; [into] starts it on that choice). */
@Composable
private fun AddChannelDialog(vm: ChatViewModel, into: Tab?, mergeInto: Tab?, onDismiss: () -> Unit) {
    var platform by rememberSaveable { mutableStateOf("twitch") }
    var channel by rememberSaveable { mutableStateOf("") }
    var merge by rememberSaveable { mutableStateOf(into != null) }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val dest = into ?: mergeInto

    fun submit() {
        if (busy) return
        busy = true; error = null
        scope.launch {
            val err = if (merge && dest != null) vm.addChannel(dest, platform, channel) else vm.addTab(platform, channel)
            busy = false
            if (err == null) onDismiss() else error = err
        }
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Add channel") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (dest != null) Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    val colors = FilterChipDefaults.filterChipColors(selectedContainerColor = AguColors.Surface2, selectedLabelColor = AguColors.Text)
                    FilterChip(selected = !merge, onClick = { merge = false }, label = { Text("New tab") }, colors = colors)
                    FilterChip(selected = merge, onClick = { merge = true }, label = { Text("Merge into ${dest.title()}", maxLines = 1) }, colors = colors)
                }
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    for (p in platforms) FilterChip(
                        selected = platform == p, onClick = { platform = p },
                        label = { Text(platformLabel(p)) },
                        leadingIcon = { Box(Modifier.size(8.dp).clip(CircleShape).background(platformColor(p))) },
                        colors = FilterChipDefaults.filterChipColors(selectedContainerColor = AguColors.Surface2, selectedLabelColor = AguColors.Text),
                    )
                }
                Box(Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(AguColors.Surface2).padding(horizontal = 14.dp, vertical = 12.dp)) {
                    if (channel.isEmpty()) Text("Channel name", color = AguColors.Muted, fontSize = 15.sp)
                    BasicTextField(
                        channel, { channel = it }, singleLine = true,
                        textStyle = TextStyle(color = AguColors.Text, fontSize = 15.sp),
                        cursorBrush = SolidColor(AguColors.Accent), modifier = Modifier.fillMaxWidth(),
                    )
                }
                error?.let { Text(it, color = AguColors.Danger, fontSize = 13.sp) }
            }
        },
        confirmButton = { TextButton(onClick = ::submit, enabled = !busy) { Text(if (busy) "Adding…" else "Add", color = AguColors.Accent) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel", color = AguColors.Muted) } },
    )
}

/**
 * Why the box is read-only, worded like the desktop's: logged out says to log in (the core's text points at Settings,
 * and the Log in button sits right beside it), otherwise the core's reason, else connecting / offline / read-only.
 */
internal fun readOnlyText(conn: Conn?, platform: String?): String? {
    if (conn == null || conn.canSend != false) return null
    val reason = conn.readOnlyReason?.replace(" in Settings", "")?.takeIf { it.isNotBlank() }
    return when {
        conn.loggedOut == true -> reason?.takeIf { it.contains("again", true) || it.contains("expired", true) } ?: "Log in to send messages"
        reason != null -> reason
        conn.state == "connecting" -> "Connecting..."
        conn.live == false && platform != "twitch" && platform != "kick" -> "Offline"
        else -> "Chat is read-only"
    }
}
