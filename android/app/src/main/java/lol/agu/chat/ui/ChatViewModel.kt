package lol.agu.chat.ui

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshots.SnapshotStateList
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import androidx.compose.runtime.snapshotFlow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import lol.agu.chat.AlertCenter
import lol.agu.chat.ChatService
import lol.agu.chat.Core
import lol.agu.chat.CoreError
import lol.agu.chat.Notifier
import lol.agu.chat.Prefs

/** State holder for the whole app: tabs, per-tab rows, connection states and accounts. Main-thread state only. */
class ChatViewModel(app: Application) : AndroidViewModel(app) {
    val tabs = mutableStateListOf<Tab>()
    /** Connection state per channel (source key). */
    val conns = mutableStateMapOf<String, Conn>()
    /** What the account may do in each channel (source key): shows the mod mode and channel modes buttons. */
    val caps = mutableStateMapOf<String, Caps>()
    /** Mod mode per tab: every line gets the mod buttons. */
    val modMode = mutableStateMapOf<String, Boolean>()
    /** Hype trains going on, by Twitch channel (source key): the bar at the top of that chat. */
    val hype = mutableStateMapOf<String, HypeTrain>()
    /** Rumble pinned messages by channel (source key): the bar at the top of that chat. */
    val pins = mutableStateMapOf<String, RumblePin>()
    /** Twitch pinned messages by channel (source key): the card at the top of that chat. */
    val twitchPins = mutableStateMapOf<String, TwitchPin>()
    /** The pins opened out to their whole text (by message id). */
    val pinsOpen = mutableStateMapOf<String, Boolean>()
    /** The tab to bring forward (one you just added); the screen scrolls to it and calls [focusDone]. A tab a whisper opens never gets it. */
    var focusTab by mutableStateOf<String?>(null)
        private set
    /** Keep the chats connected while the app is in the background (a foreground service with a notification). */
    var stayConnected by mutableStateOf(Prefs.stayConnected(app))
        private set
    /** The screen should ask for the notification permission now (the first chat was just opened). */
    var askNotifications by mutableStateOf(false)
        private set
    /** What happened in a tab you are not looking at: new messages, or a mention / highlight. */
    val unread = mutableStateMapOf<String, Dot>()
    /** The bell's count: unread mentions and highlights in any open chat. */
    var mentionsUnread by mutableStateOf(0)
        private set
    var mentionEntries by mutableStateOf<List<MentionEntry>>(emptyList())
        private set
    var mentionsOpen by mutableStateOf(false)
        private set
    var jump by mutableStateOf<Jump?>(null)
        private set
    var accounts by mutableStateOf<Map<String, AccountState>>(emptyMap())
        private set
    /** How lines are drawn (the core's display settings). */
    var display by mutableStateOf(Display())
        private set
    /** Link preview cards: rows ask for theirs, the core answers with a `preview` event. */
    val previews = PreviewStore { href -> viewModelScope.launch { runCatching { Core.call("preview.get", buildJsonObject { put("href", href) }) }.getOrNull()?.let { onPreview(it) } } }
    var appVersion by mutableStateOf("")
        private set
    /** The core's whole settings object (Settings screens read it, [patchSettings] patches it). */
    var settings by mutableStateOf(JsonObject(emptyMap()))
        private set
    /** The lists the Settings screens draw from (rule colours, badges, mod button icons, links). */
    var settingsMeta by mutableStateOf(JsonObject(emptyMap()))
        private set
    /** The updater as the Settings screen shows it. */
    var updater by mutableStateOf(UpdaterInfo())
        private set
    /** The sign-in WebView the core asked for (YouTube, X, Twitch), or null. */
    var loginRequest by mutableStateOf<LoginRequest?>(null)
        private set
    /** Errors of a login call that failed on the spot (the core's own errors come with the account state). */
    val loginErrors = mutableStateMapOf<String, String>()
    /** Platforms with a login call in flight (Rumble's password or code is being checked). */
    val loginBusy = mutableStateMapOf<String, Boolean>()

    private val alerts = AlertCenter.alerts
    private var selectedId: String? = null

    private val rowLists = HashMap<String, SnapshotStateList<Row>>()
    private val rowIndex = HashMap<String, HashMap<String, Int>>() // view key -> row key -> position

    fun rowsFor(vk: String): SnapshotStateList<Row> = rowLists.getOrPut(vk) { mutableStateListOf() }

    init {
        // Collect the core's events first so nothing is missed while the saved tabs reopen.
        viewModelScope.launch(Dispatchers.Default) {
            Core.events.collect { e ->
                when (e.kind) {
                    "rows" -> onRows(e.data)
                    "conn" -> onConn(e.data)
                    "caps" -> onCaps(e.data)
                    "activity" -> onActivity(e.data)
                    "whisper" -> onWhisper(e.data)
                    "hype" -> onHype(e.data)
                    "mentions" -> onMentions(e.data)
                    "settings" -> withContext(Dispatchers.Main) {
                        (e.data as? JsonObject)?.let { settings = it }
                        display = Display.parse(e.data, display)
                    }
                    "updater" -> withContext(Dispatchers.Main) { updater = UpdaterInfo.parse(e.data) }
                    "preview" -> onPreview(e.data)
                    "accounts" -> parseAccounts(e.data)?.let { withContext(Dispatchers.Main) { accounts = it } }
                    "login.webview", "login.show" -> onLoginRequest(e.kind, e.data)
                    "login.close", "login.forget" -> {
                        val platform = (e.data as? JsonObject)?.get("platform")?.jsonPrimitive?.contentOrNull
                        withContext(Dispatchers.Main) {
                            if (e.kind == "login.forget") LoginCookies.clear()
                            if (loginRequest?.platform == platform) loginRequest = null
                        }
                    }
                }
            }
        }
        // the background service runs while at least one chat is open (and the setting is on)
        viewModelScope.launch {
            snapshotFlow { tabs.flatMap { it.sks }.toSet().size to stayConnected }.collect { (chats, on) -> ChatService.sync(getApplication(), chats, on) }
        }
        viewModelScope.launch {
            runCatching { Core.call("settings.get") }.getOrNull()?.let { (it as? JsonObject)?.let { o -> settings = o }; display = Display.parse(it, display) }
            runCatching { Core.call("settings.meta") }.getOrNull()?.let { (it as? JsonObject)?.let { o -> settingsMeta = o } }
            runCatching { Core.call("updater.get") }.getOrNull()?.let { updater = UpdaterInfo.parse(it) }
            restoreTabs()
            refreshAccounts()
            runCatching { Core.call("app.info").jsonObject["version"]?.jsonPrimitive?.contentOrNull }
                .getOrNull()?.let { appVersion = it }
        }
    }

    // ---- events --------------------------------------------------------------------------------

    private suspend fun onRows(data: JsonElement) {
        val o = data as? JsonObject ?: return
        val vk = o["vk"]?.jsonPrimitive?.contentOrNull ?: return
        val reset = o["reset"]?.jsonPrimitive?.contentOrNull == "true"
        // Decode off the main thread; a malformed row is skipped rather than dropping the batch.
        val upsert = (o["upsert"] as? JsonArray)?.mapNotNull {
            runCatching { Core.json.decodeFromJsonElement(Row.serializer(), it) }.getOrNull()
        } ?: emptyList()
        val remove = (o["remove"] as? JsonArray)?.mapNotNull { it.jsonPrimitive.contentOrNull } ?: emptyList()
        // Rumble's pinned messages of the channels in this tab (every Rumble channel is listed; a null row = none pinned)
        val pinList = (o["pins"] as? JsonArray)?.mapNotNull { it as? JsonObject }
        val pins = pinList?.filter { it["kind"]?.jsonPrimitive?.contentOrNull != "twitch" }?.mapNotNull { po ->
            val sk = po["sk"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
            val row = (po["row"] as? JsonObject)?.let { runCatching { Core.json.decodeFromJsonElement(Row.serializer(), it) }.getOrNull() }
            Triple(sk, row, po["canMod"]?.jsonPrimitive?.contentOrNull == "true")
        }
        // Twitch's: (source key, the pin, or null when nothing is pinned, or it is hidden or over)
        val twitch = pinList?.filter { it["kind"]?.jsonPrimitive?.contentOrNull == "twitch" }?.mapNotNull { po ->
            val sk = po["sk"]?.jsonPrimitive?.contentOrNull ?: return@mapNotNull null
            val row = (po["row"] as? JsonObject)?.let { runCatching { Core.json.decodeFromJsonElement(Row.serializer(), it) }.getOrNull() }
            val by = po["by"] as? JsonObject
            sk to row?.let {
                TwitchPin(
                    po["id"]?.jsonPrimitive?.contentOrNull.orEmpty(), it, by?.get("name")?.jsonPrimitive?.contentOrNull,
                    (by?.get("badges") as? JsonArray)?.mapNotNull { b -> runCatching { Core.json.decodeFromJsonElement(Badge.serializer(), b) }.getOrNull() } ?: emptyList(),
                    po["endsAtMs"]?.jsonPrimitive?.contentOrNull?.toDoubleOrNull()?.toLong(), po["canMod"]?.jsonPrimitive?.contentOrNull == "true",
                )
            }
        }
        withContext(Dispatchers.Main) {
            pins?.forEach { (sk, row, can) -> if (row == null) this@ChatViewModel.pins.remove(sk) else this@ChatViewModel.pins[sk] = RumblePin(row, can) }
            twitch?.forEach { (sk, p) -> if (p == null) this@ChatViewModel.twitchPins.remove(sk) else this@ChatViewModel.twitchPins[sk] = p }
        }
        withContext(Dispatchers.Main) { if (tabs.any { it.id == vk }) applyRows(vk, reset, upsert, remove) }
    }

    private fun applyRows(vk: String, reset: Boolean, upsert: List<Row>, remove: List<String>) {
        val list = rowsFor(vk)
        val index = rowIndex.getOrPut(vk) { HashMap() }
        if (reset) {
            list.clear()
            index.clear()
        }
        // Replace existing keys in place, collect the new ones and append them in one go.
        val base = list.size
        val append = ArrayList<Row>()
        for (r in upsert) {
            val i = index[r.key]
            when {
                i == null -> { index[r.key] = base + append.size; append.add(r) }
                i < base -> list[i] = r
                else -> append[i - base] = r
            }
        }
        if (append.isNotEmpty()) list.addAll(append)
        if (remove.isNotEmpty()) {
            val gone = remove.toHashSet()
            if (list.removeAll { it.key in gone }) {
                index.clear()
                list.forEachIndexed { i, r -> index[r.key] = i }
            }
        }
    }

    private suspend fun onConn(data: JsonElement) {
        val o = data as? JsonObject ?: return
        val sk = o["sk"]?.jsonPrimitive?.contentOrNull ?: return
        val conn = runCatching { Core.json.decodeFromJsonElement(Conn.serializer(), o["conn"] ?: return) }.getOrNull() ?: return
        withContext(Dispatchers.Main) { conns[sk] = conn }
    }

    private suspend fun onCaps(data: JsonElement) {
        val o = data as? JsonObject ?: return
        val sk = o["sk"]?.jsonPrimitive?.contentOrNull ?: return
        val caps = Caps(o["mod"]?.jsonPrimitive?.contentOrNull == "true", o["modes"]?.jsonPrimitive?.contentOrNull == "true")
        withContext(Dispatchers.Main) { this@ChatViewModel.caps[sk] = caps }
    }

    private suspend fun onActivity(data: JsonElement) {
        val o = data as? JsonObject ?: return
        val vk = o["vk"]?.jsonPrimitive?.contentOrNull ?: return
        val dot = if (o["kind"]?.jsonPrimitive?.contentOrNull == "highlight") Dot.Highlight else Dot.Message
        withContext(Dispatchers.Main) {
            // the tab you are looking at has seen its messages
            if (vk == selectedId || tabs.none { it.id == vk }) return@withContext
            val had = unread[vk]
            if (had == null || had < dot) unread[vk] = dot
        }
    }

    private suspend fun onPreview(data: JsonElement) {
        val p = runCatching { Core.json.decodeFromJsonElement(Preview.serializer(), data) }.getOrNull() ?: return
        withContext(Dispatchers.Main) { previews.cards[p.key] = p }
    }

    private suspend fun onHype(data: JsonElement) {
        val o = data as? JsonObject ?: return
        val sk = o["sk"]?.jsonPrimitive?.contentOrNull ?: return
        val train = (o["train"] as? JsonObject)?.let { runCatching { Core.json.decodeFromJsonElement(HypeTrain.serializer(), it) }.getOrNull() }
        withContext(Dispatchers.Main) { if (train == null) hype.remove(sk) else hype[sk] = train }
    }

    /**
     * Someone whispered you (never an ignored user: the core leaves those out). A conversation with no tab yet gets one in
     * the background, marked like any tab with news: it never takes the selection or the focus.
     */
    private suspend fun onWhisper(data: JsonElement) {
        val login = (data as? JsonObject)?.get("login")?.jsonPrimitive?.contentOrNull?.takeIf { it.isNotEmpty() } ?: return
        val tab = withContext(Dispatchers.Main) {
            val ref = ChannelRef("twitch", "whisper:" + login.lowercase())
            if (tabs.any { t -> t.channels.any { it.sk() == ref.sk() } }) return@withContext null
            Tab(newId(), "", listOf(ref)).also { tabs.add(it); unread[it.id] = Dot.Highlight }
        } ?: return
        openView(tab, lenient = true, whisper = true)
        saveTabs()
    }

    private suspend fun onMentions(data: JsonElement) {
        val n = (data as? JsonObject)?.get("unread")?.jsonPrimitive?.contentOrNull?.toIntOrNull() ?: return
        withContext(Dispatchers.Main) { mentionsUnread = n }
        if (mentionsOpen) loadMentions()
    }

    private fun parseAccounts(data: JsonElement): Map<String, AccountState>? {
        val o = data as? JsonObject ?: return null
        return o.mapNotNull { (k, v) ->
            runCatching { k to Core.json.decodeFromJsonElement(AccountState.serializer(), v) }.getOrNull()
        }.toMap()
    }

    // ---- tabs ----------------------------------------------------------------------------------

    private fun newId() = "t" + java.util.UUID.randomUUID().toString().take(8)

    /** The tab you are looking at: it has seen its new messages and mentions. */
    fun select(id: String?) {
        selectedId = id
        if (id != null) unread.remove(id)
    }

    private suspend fun restoreTabs() {
        // the screen can come back after being closed while the background service kept the chats: those views are
        // closed first, the tabs below open again
        runCatching { Core.call("views.reset") }
        val saved = runCatching {
            Core.json.decodeFromJsonElement(kotlinx.serialization.builtins.ListSerializer(SavedTab.serializer()), Core.call("tabs.get"))
        }.getOrElse { emptyList() }
        // Show the tabs immediately, then open each one (a channel that fails shows its error as its status).
        val keep = saved.filter { it.channels.isNotEmpty() }
        val restored = keep.map { Tab(newId(), it.name, it.channels) }
        restored.forEachIndexed { i, t -> if (keep[i].modMode) modMode[t.id] = true }
        tabs.addAll(restored)
        for (t in restored) viewModelScope.launch { openView(t, lenient = true) }
    }

    private suspend fun openView(tab: Tab, lenient: Boolean = false, whisper: Boolean = false): String? = try {
        Core.call("view.open", buildJsonObject {
            put("vk", tab.id)
            put("channels", channelsJson(tab.channels))
            if (lenient) put("lenient", true)
            if (whisper) put("whisper", true)
        })
        null
    } catch (e: CoreError) {
        e.message ?: "Could not open the chat"
    }

    private fun channelsJson(channels: List<ChannelRef>) =
        JsonArray(channels.map { buildJsonObject { put("platform", it.platform); put("channel", it.channel) } })

    private fun clean(channelRaw: String) = channelRaw.trim().trimStart('#', '@')

    /** The channel the add dialog means: "whisper" is a Twitch whisper conversation (the core's reserved channel). */
    private fun refOf(platform: String, raw: String): ChannelRef {
        val c = clean(raw)
        return if (platform == "whisper") ChannelRef("twitch", "whisper:$c") else ChannelRef(platform, c)
    }

    private fun emptyName(platform: String) = if (platform == "whisper") "Enter a Twitch username" else "Enter a channel name"

    /** Opens a chat in a new tab; returns an error message to show, or null on success. [platform] "whisper" starts a whisper. */
    suspend fun addTab(platform: String, channelRaw: String): String? {
        if (clean(channelRaw).isEmpty()) return emptyName(platform)
        val tab = Tab(newId(), "", listOf(refOf(platform, channelRaw)))
        openView(tab)?.let { return it }
        tabs.add(tab)
        focusTab = tab.id
        saveTabs()
        // the first moment notifications matter: a chat is open, and the app can tell you about mentions
        if (!Notifier.allowed(getApplication()) && !Prefs.askedNotifications(getApplication())) askNotifications = true
        return null
    }

    /** The screen has scrolled to the tab it was told to. */
    fun focusDone(id: String) {
        if (focusTab == id) focusTab = null
    }

    /** The permission question was put to the person (once; the phone asks at most twice anyway). */
    fun notificationsAsked() {
        askNotifications = false
        Prefs.setAskedNotifications(getApplication())
    }

    /** "Stay connected in the background" on or off (the service follows). Turning it on is another moment to ask for notifications. */
    fun changeStayConnected(on: Boolean) {
        Prefs.setStayConnected(getApplication(), on)
        stayConnected = on
        if (on && !Notifier.allowed(getApplication()) && !Prefs.askedNotifications(getApplication())) askNotifications = true
    }

    /** Adds a channel to a tab (merged into its list); returns an error message to show, or null on success. */
    suspend fun addChannel(tab: Tab, platform: String, channelRaw: String): String? {
        if (clean(channelRaw).isEmpty()) return emptyName(platform)
        val ref = refOf(platform, channelRaw)
        if (tab.channels.any { it.sk() == ref.sk() }) return "That channel is already in this tab"
        // like the desktop: a whisper is its own list, it never shares one with channels
        if (tab.channels.any { it.isWhisper() != ref.isWhisper() }) return "A whisper can't share a split with channels."
        return changeChannels(tab, tab.channels + ref)
    }

    fun removeChannel(tab: Tab, ref: ChannelRef) {
        val rest = tab.channels.filter { it != ref }
        if (rest.isEmpty()) { closeTab(tab); return }
        viewModelScope.launch { changeChannels(tab, rest) }
    }

    private suspend fun changeChannels(tab: Tab, channels: List<ChannelRef>): String? {
        try {
            Core.call("view.set", buildJsonObject { put("vk", tab.id); put("channels", channelsJson(channels)) })
        } catch (e: CoreError) {
            return e.message ?: "Could not open the chat"
        }
        val i = tabs.indexOfFirst { it.id == tab.id }
        if (i >= 0) tabs[i] = tabs[i].copy(channels = channels)
        conns.keys.filter { sk -> tabs.none { sk in it.sks } }.forEach { conns.remove(it); caps.remove(it) }
        saveTabs()
        return null
    }

    /** A channel typed in the change dialog, as the add dialog reads it; null when nothing is typed. */
    fun draftRef(platform: String, raw: String): ChannelRef? = clean(raw).takeIf { it.isNotEmpty() }?.let { refOf(platform, raw) }

    /** The wording for an empty channel box. */
    fun emptyChannelText(platform: String) = emptyName(platform)

    /** The desktop's "Change channel(s)": the tab keeps its place and name and shows exactly [channels]. Null on success, else what to show. */
    suspend fun setChannels(tab: Tab, channels: List<ChannelRef>): String? {
        if (channels.isEmpty()) return "A tab needs at least one channel"
        if (channels == tab.channels) return null
        if (channels.any { it.isWhisper() } && channels.any { !it.isWhisper() }) return "A whisper can't share a split with channels."
        return changeChannels(tab, channels)
    }

    /** "Close other tabs": every tab but [keep] is closed, with its channels. */
    fun closeOthers(keep: Tab) {
        for (t in tabs.toList()) if (t.id != keep.id) closeTab(t)
        focusTab = keep.id
    }

    /** Moves a tab one place left (-1) or right (1); the order is kept with the other tabs. */
    fun moveTab(tab: Tab, by: Int) {
        val i = tabs.indexOfFirst { it.id == tab.id }
        val j = i + by
        if (i < 0 || j !in tabs.indices) return
        tabs.add(j, tabs.removeAt(i))
        focusTab = tab.id
        viewModelScope.launch { saveTabs() }
    }

    fun renameTab(tab: Tab, name: String) {
        val i = tabs.indexOfFirst { it.id == tab.id }
        if (i < 0) return
        tabs[i] = tabs[i].copy(name = name.trim())
        viewModelScope.launch { saveTabs() }
    }

    fun closeTab(tab: Tab) {
        tabs.removeAll { it.id == tab.id }
        unread.remove(tab.id)
        viewModelScope.launch {
            runCatching { Core.call("view.close", buildJsonObject { put("vk", tab.id) }) }
            rowLists.remove(tab.id)
            rowIndex.remove(tab.id)
            tab.sks.filter { sk -> tabs.none { sk in it.sks } }.forEach { conns.remove(it); caps.remove(it) }
            modMode.remove(tab.id)
            saveTabs()
        }
    }

    private suspend fun saveTabs() {
        val arr = JsonArray(tabs.map { t -> buildJsonObject { put("name", t.name); put("channels", channelsJson(t.channels)); if (modMode[t.id] == true) put("modMode", true) } })
        runCatching { Core.call("tabs.set", buildJsonObject { put("tabs", arr) }) }
    }

    /** Search a tab: only the lines that match are shown (words must all appear; `from:name` keeps one person). "" shows all. */
    fun search(vk: String, query: String) {
        viewModelScope.launch { runCatching { Core.call("view.search", buildJsonObject { put("vk", vk); put("query", query) }) } }
    }

    // ---- mentions ------------------------------------------------------------------------------

    /** The bell: opening the list marks everything read (a red tab dot goes grey: seen here). */
    fun openMentions() {
        mentionsOpen = true
        for (k in unread.keys.toList()) if (unread[k] == Dot.Highlight) unread[k] = Dot.Message
        viewModelScope.launch {
            runCatching { Core.call("mentions.open", buildJsonObject { put("open", true) }) }
            loadMentions()
        }
    }

    fun closeMentions() {
        mentionsOpen = false
        viewModelScope.launch { runCatching { Core.call("mentions.open", buildJsonObject { put("open", false) }) } }
    }

    private suspend fun loadMentions() {
        val list = runCatching { Core.call("mentions.list") as JsonArray }.getOrNull() ?: return
        val parsed = withContext(Dispatchers.Default) {
            list.mapNotNull { e ->
                runCatching {
                    val o = e.jsonObject
                    fun s(k: String) = o[k]?.jsonPrimitive?.contentOrNull.orEmpty()
                    MentionEntry(
                        s("id"), s("vk"), s("sk"), s("channel"), s("platform"), s("ts").toLongOrNull() ?: 0,
                        s("read") == "true", s("whisper") == "true",
                        Core.json.decodeFromJsonElement(Row.serializer(), o["row"]!!),
                    )
                }.getOrNull()
            }
        }
        mentionEntries = parsed
    }

    /** Takes you to a mention in its chat; false when that chat is no longer open anywhere. */
    fun jumpToMention(e: MentionEntry): Boolean {
        val tab = tabs.firstOrNull { it.id == e.vk && e.sk in it.sks } ?: tabs.firstOrNull { e.sk in it.sks } ?: return false
        closeMentions()
        jump = Jump(tab.id, e.row.key, System.nanoTime())
        return true
    }

    /** A tapped notification: bring that message's tab forward ([vk] while it is open, else any tab with [sk]) and show the line. */
    fun openFromNotification(vk: String, sk: String, key: String) {
        val tab = tabs.firstOrNull { it.id == vk } ?: tabs.firstOrNull { sk in it.sks } ?: return
        if (mentionsOpen) closeMentions()
        jump = Jump(tab.id, key, System.nanoTime())
    }

    fun jumpDone(j: Jump) {
        if (jump == j) jump = null
    }

    // ---- settings --------------------------------------------------------------------------------

    /** Change settings like the desktop does: the patch shows at once, then goes to the core, whose answer is the truth. */
    fun patchSettings(patch: JsonObject) {
        settings = JsonObject(settings + patch)
        display = Display.parse(settings, display)
        viewModelScope.launch {
            val answer = runCatching { Core.call("settings.set", buildJsonObject { put("patch", patch) }) }.getOrNull() as? JsonObject ?: return@launch
            settings = answer
            display = Display.parse(answer, display)
        }
    }

    /** The desktop's "Reload emotes": every emote set the tab's chats draw with is fetched again, past the cache. */
    fun reloadEmotes(tab: Tab) {
        viewModelScope.launch { for (ch in tab.channels) if (!ch.isWhisper()) runCatching { Core.call("chat.reloadEmotes", buildJsonObject { put("sk", ch.sk()) }) } }
    }

    /** The desktop's "Stream link": opens the page the channel's stream plays on (the core knows how each platform's channel maps to one). */
    fun openStream(ch: ChannelRef) {
        viewModelScope.launch {
            val url = runCatching { Core.call("stream.url", buildJsonObject { put("platform", ch.platform); put("channel", ch.channel) }).jsonPrimitive.contentOrNull }.getOrNull() ?: return@launch
            runCatching { Core.call("open.url", buildJsonObject { put("url", url) }) }
        }
    }

    fun updaterCheck() {
        viewModelScope.launch { runCatching { Core.call("updater.check") } }
    }

    fun updaterDownload() {
        viewModelScope.launch { runCatching { Core.call("updater.download") } }
    }

    /** Hands the verified download to the system installer; [onNeedPermission] when the phone first has to allow installs from this app. */
    fun updaterInstall(context: android.content.Context, onNeedPermission: () -> Unit) {
        if (!ApkInstaller.mayInstall(context)) return onNeedPermission()
        viewModelScope.launch {
            val path = runCatching { (Core.call("updater.install") as? JsonObject)?.get("path")?.jsonPrimitive?.contentOrNull }.getOrNull() ?: return@launch
            ApkInstaller.install(context, path)
        }
    }

    /** Plays a highlight sound once (the settings' Play buttons): a stored file, or the ping when [file] is empty. */
    fun playSound(file: String) {
        val volume = settings.int("highlightVolume", 70)
        viewModelScope.launch { alerts.play(file, volume) }
    }

    /** The app is on screen or not: a highlight vibrates only in the background. */
    fun setActive(active: Boolean) {
        viewModelScope.launch { runCatching { Core.call("app.active", buildJsonObject { put("active", active) }) } }
    }

    // ---- the message box -----------------------------------------------------------------------

    /** What you sent per tab, for the history picker (kept while the app runs, like the desktop's). */
    private val histories = HashMap<String, SentHistory>()

    fun historyOf(tabId: String): SentHistory = histories.getOrPut(tabId) { SentHistory() }

    /** Debuggable builds only (the dev frames file): put a line in a tab's sent history without sending it. */
    fun devSent(channel: String, text: String) {
        val t = tabs.firstOrNull { t -> t.channels.size == 1 && t.channels[0].platform == "twitch" && t.channels[0].channel == channel }
        t?.let { historyOf(it.id).add(text) }
    }

    /** The text in each tab's box (a draft stays when you switch tabs), and where each tab's message goes ("*" = all chats, or a source key). */
    val drafts = HashMap<String, String>()
    val targets = HashMap<String, String>()

    private fun sksJson(sks: List<String>) = JsonArray(sks.map { JsonPrimitive(it) })

    /** The strip's suggestions for [text] with the caret at [caret] (UTF-16), or null. Asks the core on its own thread. */
    suspend fun suggest(vk: String, sks: List<String>, text: String, caret: Int): Suggestions? = withContext(Dispatchers.Default) {
        runCatching {
            val r = Core.call("compose.suggest", buildJsonObject { put("vk", vk); put("sks", sksJson(sks)); put("text", text); put("caret", caret) })
            if (r is JsonNull) null else Core.json.decodeFromJsonElement(Suggestions.serializer(), r)
        }.getOrNull()
    }

    /** The emote picker's tabs and the sections of [tab] ("" = every tab); with [q] only the emotes whose name has it. */
    suspend fun picker(sks: List<String>, tab: String, q: String): PickerData = withContext(Dispatchers.Default) {
        runCatching {
            Core.json.decodeFromJsonElement(PickerData.serializer(), Core.call("compose.picker", buildJsonObject { put("sks", sksJson(sks)); put("tab", tab); put("q", q) }))
        }.getOrElse { PickerData() }
    }

    /** How the chats' modes apply to your account (slow mode, subscribers-only, ...), by source key. */
    suspend fun gates(vk: String, sks: List<String>): Map<String, Gate> = withContext(Dispatchers.Default) {
        runCatching {
            (Core.call("compose.gates", buildJsonObject { put("vk", vk); put("sks", sksJson(sks)) }) as JsonObject)
                .mapValues { Core.json.decodeFromJsonElement(Gate.serializer(), it.value) }
        }.getOrElse { emptyMap() }
    }

    private fun parseSend(r: JsonElement): SendResult {
        val o = r as? JsonObject ?: return SendResult(false)
        fun s(k: String) = o[k]?.jsonPrimitive?.contentOrNull
        val c = o["confirm"] as? JsonObject
        return SendResult(
            clear = s("clear") == "true", toast = s("toast"), warn = s("warn") == "true",
            confirm = c?.let { Confirm(it["kind"]?.jsonPrimitive?.contentOrNull.orEmpty(), it["sk"]?.jsonPrimitive?.contentOrNull.orEmpty(), it["login"]?.jsonPrimitive?.contentOrNull, it["text"]?.jsonPrimitive?.contentOrNull.orEmpty()) },
        )
    }

    /**
     * Send what the box holds to [sks], as a reply when [reply] (source key, message id) is set. The core does what the
     * desktop's box does: a slash command runs on a single Twitch chat, `/clear` and `/raid` ask first, anything else
     * is the message (cut at 500 characters) to every chat.
     */
    suspend fun composeSend(vk: String, sks: List<String>, text: String, reply: Pair<String, String>?): SendResult = try {
        parseSend(Core.call("compose.send", buildJsonObject {
            put("vk", vk); put("sks", sksJson(sks)); put("text", text)
            if (reply != null) put("reply", buildJsonObject { put("sk", reply.first); put("id", reply.second) })
        }))
    } catch (e: CoreError) {
        SendResult(false, e.message ?: "Could not send", warn = true)
    }

    /** A custom command run from a message's menu: what it stands for (that message filling the placeholders) goes to the message's chat. */
    suspend fun runCommand(vk: String, row: Row, name: String): SendResult = try {
        parseSend(Core.call("commands.run", buildJsonObject {
            put("vk", vk); put("sk", row.sk); put("name", name)
            put("login", row.user?.login.orEmpty()); put("id", row.id?.content.orEmpty()); put("text", row.plainText())
        }))
    } catch (e: CoreError) {
        SendResult(false, e.message ?: "Could not send", warn = true)
    }

    /** The confirmed `/clear` or `/raid`. */
    suspend fun composeConfirm(c: Confirm): SendResult = try {
        parseSend(Core.call("compose.confirm", buildJsonObject {
            put("sk", c.sk); put("kind", c.kind); c.login?.let { put("login", it) }
        }))
    } catch (e: CoreError) {
        SendResult(false, e.message ?: "Could not run it", warn = true)
    }

    // ---- moderation ----------------------------------------------------------------------------

    /** The account may moderate somewhere in this tab (the mod mode toggle shows). */
    fun canMod(tab: Tab) = tab.sks.any { caps[it]?.mod == true }

    /** The channels of this tab whose room settings the account may change (the shield button shows). */
    fun modesChannels(tab: Tab) = tab.channels.filter { caps[it.sk()]?.modes == true }

    fun setModMode(tab: Tab, on: Boolean) {
        modMode[tab.id] = on
        viewModelScope.launch { saveTabs() }
    }

    private suspend fun callObj(method: String, build: kotlinx.serialization.json.JsonObjectBuilder.() -> Unit = {}): JsonObject? = withContext(Dispatchers.Default) {
        try {
            Core.call(method, buildJsonObject(build)) as? JsonObject
        } catch (e: CoreError) {
            buildJsonObject { put("ok", false); put("toast", e.message ?: "failed"); put("warn", true); put("error", e.message ?: "failed") }
        }
    }

    private fun targetJson(t: ModTarget) = buildJsonObject {
        put("id", t.id); put("uid", t.uid); put("login", t.login); put("name", t.name); t.menu?.let { put("menu", it) }
    }

    private fun parseMod(o: JsonObject?): ModResult {
        o ?: return ModResult(false, "Could not do that", true)
        fun s(k: String) = o[k]?.jsonPrimitive?.contentOrNull
        return ModResult(s("ok") == "true", s("toast"), s("warn") == "true", (o["confirm"] as? JsonObject)?.get("text")?.jsonPrimitive?.contentOrNull)
    }

    /**
     * A moderation action on [t] in [sk]: ban | timeout | warn | delete | unban. Kick, Rumble and YouTube answer a ban with
     * a question first ([ModResult.confirm]); ask again with [confirmed]. The request itself is the core's.
     */
    suspend fun modAction(sk: String, action: String, t: ModTarget, seconds: Int? = null, reason: String = "", quick: Boolean = false, confirmed: Boolean = false): ModResult =
        parseMod(callObj("mod.action") {
            put("sk", sk); put("action", action); put("t", targetJson(t)); seconds?.let { put("seconds", it) }
            if (reason.isNotEmpty()) put("reason", reason)
            if (quick) put("quick", true)
            if (confirmed) put("confirmed", true)
        })

    // ---- Twitch pins and reply threads (the rules are the core's) ----------------------------------------

    /** The "Pin for" choices as (label, seconds; null = until unpinned or the stream ends). */
    suspend fun pinChoices(): List<Pair<String, Int?>> = withContext(Dispatchers.Default) {
        runCatching {
            (Core.call("pin.choices") as JsonArray).map { c ->
                val l = c as JsonArray
                l[0].jsonPrimitive.content to l[1].jsonPrimitive.contentOrNull?.toIntOrNull()
            }
        }.getOrElse { emptyList() }
    }

    /** What is left on a pin: (text like "5 min", it is over); null text when it has no end. */
    suspend fun pinLeft(endsAtMs: Long?): Pair<String?, Boolean> = withContext(Dispatchers.Default) {
        runCatching {
            val o = Core.call("pin.left", buildJsonObject { endsAtMs?.let { put("endsAtMs", it) } }) as JsonObject
            o["text"]?.jsonPrimitive?.contentOrNull to (o["over"]?.jsonPrimitive?.contentOrNull == "true")
        }.getOrElse { null to false }
    }

    /** Where "Unpin After" starts: customized time or not, the unit is minutes, the amount, and the hint under a bad amount. */
    class UnpinStart(val timeMode: Boolean, val unitMin: Boolean, val amount: String, val hint: String)

    suspend fun pinStart(endsAtMs: Long?): UnpinStart = withContext(Dispatchers.Default) {
        runCatching {
            val o = Core.call("pin.start", buildJsonObject { endsAtMs?.let { put("endsAtMs", it) } }) as JsonObject
            UnpinStart(o["timeMode"]?.jsonPrimitive?.contentOrNull == "true", o["unitMin"]?.jsonPrimitive?.contentOrNull == "true", o["amount"]?.jsonPrimitive?.contentOrNull.orEmpty(), o["hint"]?.jsonPrimitive?.contentOrNull.orEmpty())
        }.getOrElse { UnpinStart(false, true, "20", "") }
    }

    /** The seconds a customized time means, or null when Twitch would not take it (30 seconds to 30 minutes). */
    suspend fun pinSecs(amount: String, unitMin: Boolean): Int? = withContext(Dispatchers.Default) {
        runCatching { (Core.call("pin.secs", buildJsonObject { put("amount", amount); put("unitMin", unitMin) }) as? JsonPrimitive)?.contentOrNull?.toIntOrNull() }.getOrNull()
    }

    /** "Hide for Yourself": the pin goes from every tab and stays hidden. */
    fun hidePin(id: String) {
        viewModelScope.launch(Dispatchers.Default) { runCatching { Core.call("pin.hide", buildJsonObject { put("id", id) }) } }
    }

    /** The lines of the reply thread [t] names, or null when it can't be read. */
    suspend fun threadRows(t: ThreadReq): ThreadData? = withContext(Dispatchers.Default) {
        runCatching {
            val o = Core.call("thread.rows", buildJsonObject { put("vk", t.vk); put("sk", t.sk); put("parent", t.parent); put("thread", t.thread) }) as JsonObject
            ThreadData(
                o["root"]?.jsonPrimitive?.contentOrNull.orEmpty(), o["hasRoot"]?.jsonPrimitive?.contentOrNull == "true", o["canReply"]?.jsonPrimitive?.contentOrNull == "true",
                (o["rows"] as? JsonArray)?.mapNotNull { runCatching { Core.json.decodeFromJsonElement(Row.serializer(), it) }.getOrNull() } ?: emptyList(),
            )
        }.getOrNull()
    }

    /** A mod-mode command button: the template is filled in with this line and run as the message box would. */
    suspend fun modCommand(vk: String, sk: String, template: String, row: Row): SendResult {
        val o = callObj("mod.command") { put("vk", vk); put("sk", sk); put("template", template); put("t", targetJson(row.mod?.t ?: ModTarget())); put("text", row.plainText()) }
        return o?.let { parseSend(it) } ?: SendResult(false, "Could not run it", warn = true)
    }

    suspend fun modAutomod(sk: String, id: String, allow: Boolean): ModResult = parseMod(callObj("mod.automod") { put("sk", sk); put("id", id); put("allow", allow) })

    suspend fun modRules(sk: String): List<String> =
        (callObj("mod.rules") { put("sk", sk) }?.get("rules") as? JsonArray)?.mapNotNull { it.jsonPrimitive.contentOrNull } ?: emptyList()

    /** The user card's data for [user] in [sk] (the core gathers what the platform says and the lines it has seen). */
    suspend fun cardInfo(sk: String, user: RowUser): JsonObject? = callObj("card.info") {
        put("sk", sk)
        put("user", buildJsonObject { put("id", user.id.orEmpty()); put("login", user.login.orEmpty()); put("name", user.name.orEmpty()); put("avatar", user.avatar.orEmpty()) })
    }

    suspend fun cardMessages(sk: String, id: String, cursor: String?): JsonObject? = callObj("card.messages") { put("sk", sk); put("id", id); cursor?.let { put("cursor", it) } }

    suspend fun cardComment(sk: String, targetId: String, text: String): JsonObject? = callObj("card.comment") { put("sk", sk); put("targetId", targetId); put("text", text) }

    suspend fun cardCommentDelete(sk: String, id: String): JsonObject? = callObj("card.comment.delete") { put("sk", sk); put("commentId", id) }

    /** Ignore or stop ignoring a person; the toast to show. */
    suspend fun cardIgnore(platform: String, login: String, on: Boolean): String? =
        callObj("card.ignore") { put("platform", platform); put("login", login); put("on", on) }?.get("toast")?.jsonPrimitive?.contentOrNull

    /** Rumble's "Muted users" of a channel: the list, or null with the toast saying why not. */
    suspend fun rumbleMuted(sk: String): Pair<List<MutedUser>?, String?> {
        val o = callObj("rumble.muted") { put("sk", sk) } ?: return null to "Couldn't read the muted users"
        val s = { k: String -> { e: JsonElement -> (e as? JsonObject)?.get(k)?.jsonPrimitive?.contentOrNull.orEmpty() } }
        if (o["ok"]?.jsonPrimitive?.contentOrNull != "true") return null to (o["toast"]?.jsonPrimitive?.contentOrNull ?: "Couldn't read the muted users")
        val list = (o["muted"] as? JsonArray).orEmpty().map { MutedUser(s("name")(it), s("id")(it), s("scope")(it)) }
        return list to null
    }

    /** Lift one mute from that list; the answer says what to toast. */
    suspend fun rumbleUnmute(sk: String, u: MutedUser): ModResult =
        parseMod(callObj("rumble.unmute") { put("sk", sk); put("name", u.name); put("id", u.id); put("scope", u.scope) })

    suspend fun modesGet(sk: String, fresh: Boolean): JsonObject? = callObj("modes.get") { put("sk", sk); put("fresh", fresh) }
    suspend fun modesPatch(sk: String, body: JsonObject): JsonObject? = callObj("modes.patch") { put("sk", sk); put("body", body) }
    suspend fun modesShield(sk: String, on: Boolean): JsonObject? = callObj("modes.shield") { put("sk", sk); put("on", on) }
    suspend fun termsList(sk: String): JsonObject? = callObj("terms.list") { put("sk", sk) }
    suspend fun termsAdd(sk: String, text: String): JsonObject? = callObj("terms.add") { put("sk", sk); put("text", text) }
    suspend fun termsRemove(sk: String, id: String): JsonObject? = callObj("terms.remove") { put("sk", sk); put("id", id) }

    // ---- accounts ------------------------------------------------------------------------------

    private suspend fun refreshAccounts() {
        runCatching { Core.call("accounts.get") }.getOrNull()?.let { d -> parseAccounts(d)?.let { accounts = it } }
    }

    private suspend fun onLoginRequest(kind: String, data: JsonElement) {
        val o = data as? JsonObject ?: return
        val id = o["id"]?.jsonPrimitive?.contentOrNull?.toLongOrNull() ?: return
        val platform = o["platform"]?.jsonPrimitive?.contentOrNull ?: return
        val url = o["url"]?.jsonPrimitive?.contentOrNull ?: return
        withContext(Dispatchers.Main) { loginRequest = LoginRequest(id, platform, url, show = kind == "login.show") }
    }

    /** Hand the sign-in screen's outcome to the core: the cookies it found, an error, or null for "closed". */
    fun loginAnswer(request: LoginRequest, cookies: List<Pair<String, String>>? = null, error: String? = null, keepOpen: Boolean = false) {
        if (!keepOpen && loginRequest?.id == request.id) loginRequest = null
        viewModelScope.launch {
            runCatching {
                Core.call("login.result", buildJsonObject {
                    put("id", request.id)
                    when {
                        cookies != null -> put("cookies", JsonArray(cookies.map { (k, v) -> JsonArray(listOf(JsonPrimitive(k), JsonPrimitive(v))) }))
                        error != null -> put("error", error)
                        else -> put("cancelled", true)
                    }
                })
            }
            if (cookies == null && error == null) {
                // closing the window is a cancel of the whole login (also stops a Twitch flow between its two pages)
                runCatching { Core.call("accounts.cancel", buildJsonObject { put("platform", request.platform) }) }
                refreshAccounts()
            }
        }
    }

    /** Start a login; returns the core's answer (Rumble: `twoFactor`), or null when it failed (the error is in [loginErrors]). */
    suspend fun login(platform: String, args: JsonObject = JsonObject(emptyMap())): JsonObject? {
        loginErrors.remove(platform)
        loginBusy[platform] = true
        return try {
            Core.call("accounts.login", buildJsonObject { put("platform", platform); put("args", args) }) as? JsonObject ?: JsonObject(emptyMap())
        } catch (e: CoreError) {
            loginErrors[platform] = e.message?.takeIf { it.isNotBlank() } ?: "Could not log in"
            null
        } finally {
            loginBusy.remove(platform)
            refreshAccounts()
        }
    }

    fun cancelLogin(platform: String) {
        loginErrors.remove(platform)
        if (loginRequest?.platform == platform) loginRequest = null
        accountCall("accounts.cancel", platform)
    }

    fun logout(platform: String) {
        loginErrors.remove(platform)
        // the sign-in WebView's cookies go with the account, so the next login starts clean
        if (platform in listOf("twitch", "x", "youtube")) LoginCookies.clear()
        accountCall("accounts.logout", platform)
    }

    fun accountCall(method: String, platform: String) {
        viewModelScope.launch {
            runCatching { Core.call(method, buildJsonObject { put("platform", platform) }) }
            refreshAccounts()
        }
    }
}
