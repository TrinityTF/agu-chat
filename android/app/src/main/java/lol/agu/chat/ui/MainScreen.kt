package lol.agu.chat.ui

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
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
import androidx.compose.foundation.pager.PageSize
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.input.ImeAction
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
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.key
import androidx.compose.runtime.snapshotFlow
import androidx.compose.foundation.gestures.scrollBy
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
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import lol.agu.chat.R

@Composable
fun MainScreen(vm: ChatViewModel) {
    // null: closed, "": the list of sections, "accounts": straight to Accounts (a login prompt)
    var settings by rememberSaveable { mutableStateOf<String?>(null) }
    CompositionLocalProvider(LocalDisplay provides vm.display, LocalPreviews provides vm.previews) {
        Box(Modifier.fillMaxSize().background(AguColors.Bg)) {
            Chats(vm, onAccounts = { settings = "accounts" }, onSettings = { settings = "" })
            if (vm.mentionsOpen) MentionsScreen(vm)
            settings?.let { SettingsScreen(vm, start = it, onClose = { settings = null }) }
            vm.loginRequest?.let { LoginWebScreen(it, vm) }
        }
    }
}

/** Where the add-channel dialog puts the channel: a new tab, or merged into [tab]. [whisper] opens it on the whisper choice. */
private class AddTo(val tab: Tab?, val whisper: Boolean = false)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Chats(vm: ChatViewModel, onAccounts: () -> Unit, onSettings: () -> Unit) {
    val tabs = vm.tabs
    val pager = rememberPagerState { tabs.size }
    // search: the text typed per tab (only the open tab's search box shows); a wide screen can show two tabs side by side
    var searching by rememberSaveable { mutableStateOf(false) }
    val queries = remember { HashMap<String, String>() }
    var query by remember { mutableStateOf("") }
    var split by rememberSaveable { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }
    val ctx = LocalContext.current
    // moderation: what happened shows in the snackbar (the latest one replaces the one before)
    val mod = remember(vm) {
        ModController(vm, scope) { text -> scope.launch { snackbar.currentSnackbarData?.dismiss(); snackbar.showSnackbar(text) } }
    }
    var modesFor by remember { mutableStateOf<List<ChannelRef>?>(null) }

    var adding by remember { mutableStateOf<AddTo?>(null) }
    var tabMenu by remember { mutableStateOf<Tab?>(null) }
    var renaming by remember { mutableStateOf<Tab?>(null) }
    var menuRow by remember { mutableStateOf<Row?>(null) }
    // the reply thread over the chat
    var threadReq by remember { mutableStateOf<ThreadReq?>(null) }
    var replyTo by remember { mutableStateOf<Row?>(null) }
    // a name for the message box (a Super Chat's Reply puts @name there)
    var mention by remember { mutableStateOf<String?>(null) }

    val current = tabs.getOrNull(pager.currentPage)

    // Jump to a tab you just added (a tab a whisper opens stays in the background, and so do restored ones).
    val focus = vm.focusTab
    LaunchedEffect(focus) {
        val i = tabs.indexOfFirst { it.id == focus }
        if (focus != null && i >= 0) pager.animateScrollToPage(i)
        if (focus != null) vm.focusDone(focus)
    }
    // The first chat you open is the moment to ask for the notification permission (mentions while the app is closed).
    val askPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { }
    LaunchedEffect(vm.askNotifications) {
        if (vm.askNotifications) {
            vm.notificationsAsked()
            if (android.os.Build.VERSION.SDK_INT >= 33) runCatching { askPermission.launch(Manifest.permission.POST_NOTIFICATIONS) }
        }
    }
    // Each tab keeps its own draft; replying is per tab too. The tab you look at has seen its messages.
    LaunchedEffect(current?.id) { replyTo = null; vm.select(current?.id); query = queries[current?.id].orEmpty() }
    // A tapped mention: its tab comes forward (the list scrolls to the message itself)
    val jump = vm.jump
    LaunchedEffect(jump) {
        val i = tabs.indexOfFirst { it.id == jump?.vk }
        if (i >= 0 && i != pager.currentPage) pager.animateScrollToPage(i)
    }

    // a tap on a reply's "Replying to" line opens its thread, in the tab that line is in
    val openThread: (Row) -> Unit = { row ->
        val t = tabs.firstOrNull { it.id == current?.id && row.sk in it.sks } ?: tabs.firstOrNull { row.sk in it.sks }
        if (t != null) threadReq = threadOf(t.id, row, fromReplyLine = true)
    }
    CompositionLocalProvider(LocalMod provides mod, LocalThreadOpener provides openThread) {
    BoxWithConstraints(Modifier.fillMaxSize()) {
    val width: Dp = maxWidth
    val wide = width >= 840.dp
    Column(Modifier.fillMaxSize().statusBarsPadding().imePadding()) {
        TopBar(
            onSettings, vm.mentionsUnread, onBell = { vm.openMentions() },
            searching = searching, onSearch = {
                searching = !searching
                if (!searching) { current?.let { t -> queries[t.id] = ""; vm.search(t.id, "") }; query = "" }
            },
            showSplit = tabs.size > 1 && wide, split = split, onSplit = { split = !split },
            showMod = current != null && vm.canMod(current), modOn = current != null && vm.modMode[current.id] == true,
            onMod = { current?.let { t -> vm.setModMode(t, vm.modMode[t.id] != true) } },
            showModes = current != null && vm.modesChannels(current).isNotEmpty(),
            onModes = { current?.let { t -> modesFor = vm.modesChannels(t) } },
        )
        if (searching) SearchBar(
            query = query,
            onQuery = { q -> query = q; current?.let { t -> queries[t.id] = q; vm.search(t.id, q) } },
            onClose = { searching = false; current?.let { t -> queries[t.id] = ""; vm.search(t.id, "") }; query = "" },
        )
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
                    // like the desktop's empty tab: a whisper can be started from here too
                    Row(
                        Modifier.clip(RoundedCornerShape(22.dp)).background(AguColors.Surface2).clickable { adding = AddTo(null, whisper = true) }.padding(horizontal = 18.dp, vertical = 10.dp),
                        verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        MailIcon(16)
                        Text("Whisper", color = AguColors.Text, fontSize = 15.sp)
                    }
                }
            }
        } else {
            val two = wide && split && tabs.size > 1
            HorizontalPager(
                pager, Modifier.weight(1f), key = { tabs.getOrNull(it)?.id ?: it },
                pageSize = if (two) PageSize.Fixed(width / 2) else PageSize.Fill,
            ) { page ->
                val tab = tabs.getOrNull(page)
                if (tab != null) Box(Modifier.fillMaxSize().let { if (two && page > pager.currentPage) it.drawBehind { drawRect(AguColors.Surface2, size = Size(1.dp.toPx(), size.height)) } else it }) {
                    ChatList(
                        vm, tab, jump?.takeIf { it.vk == tab.id }, onLongPress = { menuRow = it },
                        searching = searching && tab.id == current?.id && query.isNotBlank(),
                        onReplyNotice = { r -> r.user?.let { u -> (u.name ?: u.login)?.let { n -> mention = n } } },
                    )
                }
            }
            current?.let { tab ->
                key(tab.id) {
                    ComposerHost(
                        vm, tab, replyTo, onClearReply = { replyTo = null },
                        mention = mention, onMentionUsed = { mention = null },
                        snackbar = snackbar, onLogin = onAccounts, modifier = Modifier.navigationBarsPadding(),
                    )
                }
            }
        }
        SnackbarHost(snackbar)
    }
    }
    }

    adding?.let { a ->
        AddChannelDialog(vm, into = a.tab, mergeInto = current, startWhisper = a.whisper, onDismiss = { adding = null })
    }

    tabMenu?.let { tab ->
        ModalBottomSheet(onDismissRequest = { tabMenu = null }, containerColor = AguColors.Surface) {
            Column(Modifier.navigationBarsPadding().padding(bottom = 12.dp)) {
                Text(tab.title(full = true), color = AguColors.Muted, fontSize = 13.sp, maxLines = 1, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
                SheetItem("Rename") { renaming = tab; tabMenu = null }
                SheetItem("Add a channel to this tab") { adding = AddTo(tab); tabMenu = null }
                if (!tab.whisperOnly) SheetItem("Reload emotes") { vm.reloadEmotes(tab); tabMenu = null }
                for (ch in tab.channels) if (!ch.isWhisper()) SheetItem("Stream link: ${ch.label()}") { vm.openStream(ch); tabMenu = null }
                if (tab.channels.size > 1) for (ch in tab.channels) {
                    SheetItem("Remove ${ch.label()}") { vm.removeChannel(tab, ch); tabMenu = null }
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

    menuRow?.let { row ->
        RowActionsSheet(
            mod, row,
            onReply = if ((row.kind == "msg" || row.kind == "notice") && !isWhisperSk(row.sk)) ({ replyTo = row; menuRow = null }) else null,
            onCopy = { copyText(ctx, row.plainText()); menuRow = null },
            onDismiss = { menuRow = null },
            onThread = if (canThread(row)) ({
                val t = tabs.firstOrNull { it.id == current?.id && row.sk in it.sks } ?: tabs.firstOrNull { row.sk in it.sks }
                if (t != null) threadReq = threadOf(t.id, row, fromReplyLine = false)
                menuRow = null
            }) else null,
        )
    }

    threadReq?.let { t -> ThreadSheet(vm, t, onDismiss = { threadReq = null }, onLogin = onAccounts) }

    ModDialogs(mod)
    mod.card?.let { req -> UserCardSheet(vm, mod, req, onDismiss = { mod.card = null }, onLogin = onAccounts) }
    modesFor?.let { list -> if (list.isNotEmpty()) ModesSheet(vm, list, onDismiss = { modesFor = null }) }
}

@Composable
private fun SheetItem(label: String, color: Color = AguColors.Text, onClick: () -> Unit) {
    Text(label, color = color, fontSize = 16.sp, modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 20.dp, vertical = 14.dp))
}

@Composable
internal fun TargetRow(label: String, platform: String?, reason: String?, selected: Boolean, enabled: Boolean, onClick: () -> Unit) {
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
private fun TopBar(
    onSettings: () -> Unit, mentions: Int, onBell: () -> Unit,
    searching: Boolean = false, onSearch: () -> Unit = {},
    showSplit: Boolean = false, split: Boolean = false, onSplit: () -> Unit = {},
    showMod: Boolean = false, modOn: Boolean = false, onMod: () -> Unit = {},
    showModes: Boolean = false, onModes: () -> Unit = {},
) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(30.dp).clip(RoundedCornerShape(9.dp)).background(AguColors.Brand), contentAlignment = Alignment.Center) {
            // the launcher glyph (40 of its 108 units): drawn at 45dp its bubbles fill ~55% of the square, as on agu.lol/app
            Image(painterResource(R.drawable.ic_launcher_foreground), contentDescription = null, modifier = Modifier.requiredSize(45.dp))
        }
        Text("Agu Chat", color = AguColors.Text, fontSize = 19.sp, fontWeight = FontWeight.Bold, modifier = Modifier.padding(start = 10.dp).weight(1f))
        if (showModes) Box(Modifier.size(40.dp).clip(CircleShape).clickable(onClick = onModes), contentAlignment = Alignment.Center) { ModIcon("shield", AguColors.Text, 22) }
        // mod mode: the sword
        if (showMod) Box(
            Modifier.size(40.dp).clip(CircleShape).background(if (modOn) AguColors.Accent else Color.Transparent).clickable(onClick = onMod),
            contentAlignment = Alignment.Center,
        ) {
            Canvas(Modifier.size(22.dp)) {
                val c = if (modOn) Color.White else AguColors.Text
                val w = size.width
                val sw = w * 0.1f
                drawLine(c, Offset(w * 0.2f, w * 0.8f), Offset(w * 0.8f, w * 0.2f), strokeWidth = sw, cap = androidx.compose.ui.graphics.StrokeCap.Round)
                drawLine(c, Offset(w * 0.8f, w * 0.2f), Offset(w * 0.8f, w * 0.38f), strokeWidth = sw, cap = androidx.compose.ui.graphics.StrokeCap.Round)
                drawLine(c, Offset(w * 0.8f, w * 0.2f), Offset(w * 0.62f, w * 0.2f), strokeWidth = sw, cap = androidx.compose.ui.graphics.StrokeCap.Round)
                drawLine(c, Offset(w * 0.18f, w * 0.5f), Offset(w * 0.5f, w * 0.82f), strokeWidth = sw, cap = androidx.compose.ui.graphics.StrokeCap.Round)
                drawLine(c, Offset(w * 0.2f, w * 0.8f), Offset(w * 0.1f, w * 0.9f), strokeWidth = sw, cap = androidx.compose.ui.graphics.StrokeCap.Round)
            }
        }
        if (showSplit) Box(Modifier.size(40.dp).clip(CircleShape).clickable(onClick = onSplit), contentAlignment = Alignment.Center) {
            // two panes side by side
            Canvas(Modifier.size(22.dp)) {
                val c = if (split) AguColors.Accent else AguColors.Text
                val w = size.width
                drawRect(c, Offset(w * 0.06f, w * 0.18f), Size(w * 0.42f, w * 0.64f), style = androidx.compose.ui.graphics.drawscope.Stroke(width = w * 0.09f))
                drawRect(c, Offset(w * 0.52f, w * 0.18f), Size(w * 0.42f, w * 0.64f), style = androidx.compose.ui.graphics.drawscope.Stroke(width = w * 0.09f))
            }
        }
        Box(Modifier.size(40.dp).clip(CircleShape).clickable(onClick = onSearch), contentAlignment = Alignment.Center) {
            // a magnifier
            Canvas(Modifier.size(22.dp)) {
                val c = if (searching) AguColors.Accent else AguColors.Text
                val w = size.width
                drawCircle(c, radius = w * 0.28f, center = Offset(w * 0.42f, w * 0.42f), style = androidx.compose.ui.graphics.drawscope.Stroke(width = w * 0.1f))
                drawLine(c, Offset(w * 0.64f, w * 0.64f), Offset(w * 0.9f, w * 0.9f), strokeWidth = w * 0.11f, cap = androidx.compose.ui.graphics.StrokeCap.Round)
            }
        }
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
        Box(Modifier.size(40.dp).clip(CircleShape).clickable(onClick = onSettings), contentAlignment = Alignment.Center) {
            // a gear: a ring with eight teeth
            Canvas(Modifier.size(22.dp)) {
                val c = AguColors.Text
                val w = size.width
                val mid = Offset(w / 2, w / 2)
                drawCircle(c, radius = w * 0.24f, center = mid, style = androidx.compose.ui.graphics.drawscope.Stroke(width = w * 0.11f))
                for (k in 0 until 8) {
                    val a = Math.toRadians(k * 45.0)
                    val dx = Math.cos(a).toFloat()
                    val dy = Math.sin(a).toFloat()
                    drawLine(c, Offset(mid.x + dx * w * 0.32f, mid.y + dy * w * 0.32f), Offset(mid.x + dx * w * 0.46f, mid.y + dy * w * 0.46f), strokeWidth = w * 0.14f, cap = androidx.compose.ui.graphics.StrokeCap.Round)
                }
            }
        }
    }
}

/** Search a tab: words that must all appear, and `from:name` for one person's messages. */
@Composable
private fun SearchBar(query: String, onQuery: (String) -> Unit, onClose: () -> Unit) {
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
    Row(Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 4.dp).clip(RoundedCornerShape(22.dp)).background(AguColors.Surface2).padding(start = 16.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.weight(1f).padding(vertical = 10.dp)) {
            if (query.isEmpty()) Text("Search messages, from:name", color = AguColors.Muted, fontSize = 15.sp)
            BasicTextField(
                query, onQuery, singleLine = true,
                textStyle = TextStyle(color = AguColors.Text, fontSize = 15.sp),
                cursorBrush = SolidColor(AguColors.Accent),
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                keyboardActions = KeyboardActions(onSearch = {}),
                modifier = Modifier.fillMaxWidth().focusRequester(focus),
            )
        }
        Text("×", color = AguColors.Text, fontSize = 22.sp, modifier = Modifier.clip(CircleShape).clickable(onClick = onClose).padding(horizontal = 14.dp))
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
                    if (tab.whisperOnly) MailIcon(13, modifier = Modifier.padding(end = 3.dp))
                    else for (ch in tab.channels.take(3)) Box(Modifier.padding(end = 3.dp).size(7.dp).clip(CircleShape).background(platformColor(ch.platform)))
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
private fun ChatList(vm: ChatViewModel, tab: Tab, jump: Jump?, onLongPress: (Row) -> Unit, searching: Boolean = false, onReplyNotice: ((Row) -> Unit)? = null) {
    val rows = vm.rowsFor(tab.id)
    val state = rememberLazyListState()
    val scope = rememberCoroutineScope()
    // "At the bottom" is judged from the previous layout, so a freshly appended row does not unstick us.
    val atBottom by remember { derivedStateOf { !state.canScrollForward } }
    // how many lines there were when you scrolled up (the pill counts what came since)
    var pausedAt by remember { mutableStateOf<Int?>(null) }
    // Stuck to the bottom until you scroll up; late-loading pictures and cards then keep it at the bottom.
    var stuck by remember { mutableStateOf(true) }
    LaunchedEffect(state) {
        snapshotFlow { state.isScrollInProgress }.collect { moving -> if (!moving) stuck = !state.canScrollForward }
    }
    LaunchedEffect(stuck) { pausedAt = if (stuck) null else rows.size }
    var flashKey by remember { mutableStateOf<String?>(null) }
    val merged = tab.channels.size > 1

    LaunchedEffect(state) {
        snapshotFlow { state.layoutInfo.totalItemsCount to state.layoutInfo.visibleItemsInfo.lastOrNull()?.let { it.offset + it.size } }.collect {
            if (stuck && rows.isNotEmpty() && state.canScrollForward) {
                state.scrollToItem(rows.lastIndex)
                state.scrollBy(100000f)
            }
        }
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

    val modOn = vm.modMode[tab.id] == true && LocalDisplay.current.showModButtons
    CompositionLocalProvider(LocalModTab provides tab, LocalModMode provides modOn) {
    Column(Modifier.fillMaxSize()) {
        // the hype train bars (a tap opens one's details, drawn over the list below)
        var hypeOpen by remember { mutableStateOf<String?>(null) }
        HypeBars(tab, vm.hype, setOfNotNull(hypeOpen)) { sk -> hypeOpen = if (hypeOpen == sk) null else sk }
        PinnedBars(tab, vm, LocalMod.current)
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
            Triple(if (merged) "${if (ch.isWhisper()) "Whisper" else platformLabel(ch.platform)}: $label" else label, conn.detail, ch.sk())
        }
        if (lines.isNotEmpty()) {
            Column(Modifier.fillMaxWidth().background(AguColors.Surface).padding(horizontal = 14.dp, vertical = 6.dp)) {
                for ((label, detail, _) in lines) {
                    Text(label, color = AguColors.Muted, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                    if (!detail.isNullOrBlank()) Text(detail, color = AguColors.Muted, fontSize = 12.sp)
                }
            }
        }
    Box(Modifier.weight(1f).fillMaxWidth()) {
        LazyColumn(Modifier.fillMaxSize(), state) {
            items(rows, key = { it.key }) { row ->
                MessageRow(row, onLongPress, showMark = merged && LocalDisplay.current.showPlatformMarks, flash = row.key == flashKey, onReplyNotice = onReplyNotice)
            }
        }
        run {
            val bars = tab.channels.count { !it.isWhisper() && vm.hype.containsKey(it.sk()) }
            // the details sit under the bars (the bars are 52 dp each, 6 dp apart, 6 dp from the top) and over the messages: the box starts above the list by that much
            if (bars > 0) HypeDetails(tab, vm.hype, setOfNotNull(hypeOpen), 0, Modifier.align(Alignment.TopCenter))
        }
        if (!stuck && !atBottom && rows.isNotEmpty()) {
            val more = pausedAt?.let { (rows.size - it).coerceAtLeast(0) } ?: 0
            Text(
                "${if (searching) "Newest matches" else "More messages below"}${if (more > 0) " (${if (more > 999) "999+" else more})" else ""}", color = Color.White, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 12.dp)
                    .clip(RoundedCornerShape(50)).background(AguColors.Accent)
                    .clickable { stuck = true; scope.launch { state.scrollToItem(rows.lastIndex); state.scrollBy(100000f) } }
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
    }
    }
    }
}

/**
 * Add a channel: to a new tab, or merged into the tab you are on ([mergeInto]; [into] starts it on that choice). The
 * Whisper choice opens a Twitch whisper conversation with a username instead ([startWhisper] starts on it).
 */
@Composable
private fun AddChannelDialog(vm: ChatViewModel, into: Tab?, mergeInto: Tab?, startWhisper: Boolean, onDismiss: () -> Unit) {
    var platform by rememberSaveable { mutableStateOf(if (startWhisper) "whisper" else "twitch") }
    var channel by rememberSaveable { mutableStateOf("") }
    var merge by rememberSaveable { mutableStateOf(into != null) }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val dest = into ?: mergeInto
    val whisper = platform == "whisper"

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
        title = { Text(if (whisper) "Whisper someone" else "Add channel") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (dest != null) Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    val colors = FilterChipDefaults.filterChipColors(selectedContainerColor = AguColors.Surface2, selectedLabelColor = AguColors.Text)
                    FilterChip(selected = !merge, onClick = { merge = false }, label = { Text("New tab") }, colors = colors)
                    FilterChip(selected = merge, onClick = { merge = true }, label = { Text("Merge into ${dest.title()}", maxLines = 1) }, colors = colors)
                }
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    val colors = FilterChipDefaults.filterChipColors(selectedContainerColor = AguColors.Surface2, selectedLabelColor = AguColors.Text)
                    for (p in platforms) FilterChip(
                        selected = platform == p, onClick = { platform = p },
                        label = { Text(platformLabel(p)) },
                        leadingIcon = { Box(Modifier.size(8.dp).clip(CircleShape).background(platformColor(p))) },
                        colors = colors,
                    )
                    FilterChip(selected = whisper, onClick = { platform = "whisper" }, label = { Text("Whisper") }, leadingIcon = { MailIcon(14) }, colors = colors)
                }
                Box(Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp)).background(AguColors.Surface2).padding(horizontal = 14.dp, vertical = 12.dp)) {
                    if (channel.isEmpty()) Text(if (whisper) "username" else "Channel name", color = AguColors.Muted, fontSize = 15.sp)
                    BasicTextField(
                        channel, { channel = it }, singleLine = true,
                        textStyle = TextStyle(color = AguColors.Text, fontSize = 15.sp),
                        cursorBrush = SolidColor(AguColors.Accent), modifier = Modifier.fillMaxWidth(),
                    )
                }
                if (whisper) Text(WHISPER_HINT, color = AguColors.Muted, fontSize = 12.sp)
                error?.let { Text(it, color = AguColors.Danger, fontSize = 13.sp) }
            }
        },
        confirmButton = { TextButton(onClick = ::submit, enabled = !busy) { Text(if (busy) "Adding…" else if (whisper) "Open" else "Add", color = AguColors.Accent) } },
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
