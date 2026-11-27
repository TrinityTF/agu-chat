package lol.agu.chat.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.ui.res.painterResource
import lol.agu.chat.R
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch

@Composable
fun MainScreen(vm: ChatViewModel) {
    var showAccounts by rememberSaveable { mutableStateOf(false) }
    Box(Modifier.fillMaxSize().background(AguColors.Bg)) {
        Chats(vm, onAccounts = { showAccounts = true })
        if (showAccounts) AccountsScreen(vm, onBack = { showAccounts = false })
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Chats(vm: ChatViewModel, onAccounts: () -> Unit) {
    val tabs = vm.tabs
    val pager = rememberPagerState { tabs.size }
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }
    val ctx = LocalContext.current

    var showAdd by rememberSaveable { mutableStateOf(false) }
    var closing by remember { mutableStateOf<Tab?>(null) }
    var menuRow by remember { mutableStateOf<Row?>(null) }
    var replyTo by remember { mutableStateOf<Row?>(null) }
    val drafts = remember { HashMap<String, String>() }
    var text by remember { mutableStateOf("") }

    val current = tabs.getOrNull(pager.currentPage)

    // Jump to a newly added tab (but not when the saved tabs are first restored).
    var prevCount by remember { mutableStateOf(tabs.size) }
    LaunchedEffect(tabs.size) {
        if (tabs.size > prevCount && prevCount > 0) pager.animateScrollToPage(tabs.size - 1)
        prevCount = tabs.size
    }
    // Each tab keeps its own draft; replying is per tab too.
    LaunchedEffect(current?.id) { replyTo = null; text = drafts[current?.id].orEmpty() }

    Column(Modifier.fillMaxSize().statusBarsPadding().imePadding()) {
        TopBar(onAccounts)
        TabStrip(
            tabs = tabs, selected = pager.currentPage,
            onSelect = { i -> scope.launch { pager.animateScrollToPage(i) } },
            onClose = { closing = it },
            onAdd = { showAdd = true },
        )
        if (tabs.isEmpty()) {
            Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    Text("Add a channel to start", color = AguColors.Muted, fontSize = 16.sp)
                    Box(
                        Modifier.size(56.dp).clip(CircleShape).background(AguColors.Brand).clickable { showAdd = true },
                        contentAlignment = Alignment.Center,
                    ) { Text("+", color = Color.White, fontSize = 30.sp) }
                }
            }
        } else {
            HorizontalPager(pager, Modifier.weight(1f), key = { tabs.getOrNull(it)?.id ?: it }) { page ->
                val tab = tabs.getOrNull(page)
                if (tab != null) ChatList(vm, tab, onLongPress = { menuRow = it })
            }
            val conn = current?.let { vm.conns[it.sk] }
            Composer(
                text = text,
                onText = { text = it; current?.let { t -> drafts[t.id] = it } },
                canSend = conn?.canSend != false,
                // the core words it for the desktop app ("... in Settings"); here the Log in button is right beside it
                readOnlyReason = if (conn?.loggedOut == true) "Log in to send messages" else conn?.readOnlyReason,
                replyingTo = replyTo?.let { it.user?.name ?: it.user?.login },
                onCancelReply = { replyTo = null },
                onSend = {
                    val tab = current ?: return@Composer
                    val msg = text.trim()
                    if (msg.isEmpty()) return@Composer
                    scope.launch {
                        val err = vm.send(tab.sk, msg, replyTo?.id?.content)
                        if (err == null) { text = ""; drafts[tab.id] = ""; replyTo = null } else snackbar.showSnackbar(err)
                    }
                },
                onLogin = onAccounts,
                modifier = Modifier.navigationBarsPadding(),
            )
        }
        SnackbarHost(snackbar)
    }

    if (showAdd) AddChannelDialog(vm, onDismiss = { showAdd = false })

    closing?.let { tab ->
        AlertDialog(
            onDismissRequest = { closing = null },
            title = { Text("Close ${tab.channel}?") },
            text = { Text("The chat tab will be removed.") },
            confirmButton = { TextButton(onClick = { vm.closeTab(tab); closing = null }) { Text("Close", color = AguColors.Accent) } },
            dismissButton = { TextButton(onClick = { closing = null }) { Text("Cancel", color = AguColors.Muted) } },
        )
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
private fun SheetItem(label: String, onClick: () -> Unit) {
    Text(label, color = AguColors.Text, fontSize = 16.sp, modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 20.dp, vertical = 14.dp))
}

private fun copyText(ctx: Context, text: String) {
    (ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("message", text))
}

@Composable
private fun TopBar(onAccounts: () -> Unit) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(30.dp).clip(RoundedCornerShape(9.dp)).background(AguColors.Brand), contentAlignment = Alignment.Center) {
            // the launcher glyph (40 of its 108 units): drawn at 45dp its bubbles fill ~55% of the square, as on agu.lol/app
            Image(painterResource(R.drawable.ic_launcher_foreground), contentDescription = null, modifier = Modifier.requiredSize(45.dp))
        }
        Text("Agu Chat", color = AguColors.Text, fontSize = 19.sp, fontWeight = FontWeight.Bold, modifier = Modifier.padding(start = 10.dp).weight(1f))
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

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun TabStrip(tabs: List<Tab>, selected: Int, onSelect: (Int) -> Unit, onClose: (Tab) -> Unit, onAdd: () -> Unit) {
    Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 6.dp)) {
        tabs.forEachIndexed { i, tab ->
            val sel = i == selected
            Column(
                Modifier.combinedClickable(onClick = { onSelect(i) }, onLongClick = { onClose(tab) }).padding(horizontal = 6.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Row(
                    Modifier.height(42.dp).drawBehind {
                        if (sel) drawRect(AguColors.Accent, Offset(0f, size.height - 2.dp.toPx()), Size(size.width, 2.dp.toPx()))
                    }.padding(horizontal = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Box(Modifier.size(8.dp).clip(CircleShape).background(platformColor(tab.platform)))
                    Text(
                        tab.channel, color = if (sel) AguColors.Text else AguColors.Muted, fontSize = 14.sp,
                        fontWeight = if (sel) FontWeight.SemiBold else FontWeight.Normal, modifier = Modifier.padding(start = 7.dp),
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
private fun ChatList(vm: ChatViewModel, tab: Tab, onLongPress: (Row) -> Unit) {
    val rows = vm.rowsFor(tab.sk)
    val conn = vm.conns[tab.sk]
    val state = rememberLazyListState()
    val scope = rememberCoroutineScope()
    // "At the bottom" is judged from the previous layout, so a freshly appended row does not unstick us.
    val atBottom by remember { derivedStateOf { !state.canScrollForward } }

    LaunchedEffect(rows.size) {
        if (atBottom && rows.isNotEmpty()) state.scrollToItem(rows.lastIndex)
    }

    Box(Modifier.fillMaxSize()) {
        LazyColumn(Modifier.fillMaxSize(), state) {
            items(rows, key = { it.key }) { row -> MessageRow(row, onLongPress) }
        }
        if (conn != null && conn.state != "open") {
            val label = when (conn.state) {
                "connecting" -> "Connecting…"
                "offline" -> "Offline"
                "error" -> "Connection error"
                "unavailable" -> "Unavailable"
                else -> conn.state
            }
            Column(Modifier.fillMaxWidth().background(AguColors.Surface.copy(alpha = 0.95f)).padding(horizontal = 14.dp, vertical = 6.dp)) {
                Text(label, color = AguColors.Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                if (!conn.detail.isNullOrBlank()) Text(conn.detail, color = AguColors.Muted, fontSize = 12.sp)
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

@Composable
private fun AddChannelDialog(vm: ChatViewModel, onDismiss: () -> Unit) {
    var platform by rememberSaveable { mutableStateOf("twitch") }
    var channel by rememberSaveable { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    fun submit() {
        if (busy) return
        busy = true; error = null
        scope.launch {
            val err = vm.addTab(platform, channel)
            busy = false
            if (err == null) onDismiss() else error = err
        }
    }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Add channel") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
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
