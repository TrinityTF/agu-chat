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
import lol.agu.chat.Core
import lol.agu.chat.CoreError

/** State holder for the whole app: tabs, per-tab rows, connection states and accounts. Main-thread state only. */
class ChatViewModel(app: Application) : AndroidViewModel(app) {
    val tabs = mutableStateListOf<Tab>()
    /** Connection state per channel (source key). */
    val conns = mutableStateMapOf<String, Conn>()
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
    /** The sign-in WebView the core asked for (YouTube, X, Twitch), or null. */
    var loginRequest by mutableStateOf<LoginRequest?>(null)
        private set
    /** Errors of a login call that failed on the spot (the core's own errors come with the account state). */
    val loginErrors = mutableStateMapOf<String, String>()
    /** Platforms with a login call in flight (Rumble's password or code is being checked). */
    val loginBusy = mutableStateMapOf<String, Boolean>()

    private val alerts = Alerts(app)
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
                    "activity" -> onActivity(e.data)
                    "alert" -> alerts.fire(e.data)
                    "mentions" -> onMentions(e.data)
                    "settings" -> withContext(Dispatchers.Main) { display = Display.parse(e.data, display) }
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
        viewModelScope.launch {
            runCatching { Core.call("settings.get") }.getOrNull()?.let { display = Display.parse(it, display) }
            alerts.prepare()
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
        val saved = runCatching {
            Core.json.decodeFromJsonElement(kotlinx.serialization.builtins.ListSerializer(SavedTab.serializer()), Core.call("tabs.get"))
        }.getOrElse { emptyList() }
        // Show the tabs immediately, then open each one (a channel that fails shows its error as its status).
        val restored = saved.filter { it.channels.isNotEmpty() }.map { Tab(newId(), it.name, it.channels) }
        tabs.addAll(restored)
        for (t in restored) viewModelScope.launch { openView(t, lenient = true) }
    }

    private suspend fun openView(tab: Tab, lenient: Boolean = false): String? = try {
        Core.call("view.open", buildJsonObject {
            put("vk", tab.id)
            put("channels", channelsJson(tab.channels))
            if (lenient) put("lenient", true)
        })
        null
    } catch (e: CoreError) {
        e.message ?: "Could not open the chat"
    }

    private fun channelsJson(channels: List<ChannelRef>) =
        JsonArray(channels.map { buildJsonObject { put("platform", it.platform); put("channel", it.channel) } })

    private fun clean(channelRaw: String) = channelRaw.trim().trimStart('#', '@')

    /** Opens a chat in a new tab; returns an error message to show, or null on success. */
    suspend fun addTab(platform: String, channelRaw: String): String? {
        val channel = clean(channelRaw)
        if (channel.isEmpty()) return "Enter a channel name"
        val tab = Tab(newId(), "", listOf(ChannelRef(platform, channel)))
        openView(tab)?.let { return it }
        tabs.add(tab)
        saveTabs()
        return null
    }

    /** Adds a channel to a tab (merged into its list); returns an error message to show, or null on success. */
    suspend fun addChannel(tab: Tab, platform: String, channelRaw: String): String? {
        val channel = clean(channelRaw)
        if (channel.isEmpty()) return "Enter a channel name"
        val ref = ChannelRef(platform, channel)
        if (tab.channels.any { it.sk() == ref.sk() }) return "That channel is already in this tab"
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
        conns.keys.filter { sk -> tabs.none { sk in it.sks } }.forEach { conns.remove(it) }
        saveTabs()
        return null
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
            tab.sks.filter { sk -> tabs.none { sk in it.sks } }.forEach { conns.remove(it) }
            saveTabs()
        }
    }

    private suspend fun saveTabs() {
        val arr = JsonArray(tabs.map { buildJsonObject { put("name", it.name); put("channels", channelsJson(it.channels)) } })
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

    fun jumpDone(j: Jump) {
        if (jump == j) jump = null
    }

    /** The app is on screen or not: a highlight vibrates only in the background. */
    fun setActive(active: Boolean) {
        viewModelScope.launch { runCatching { Core.call("app.active", buildJsonObject { put("active", active) }) } }
    }

    // ---- sending -------------------------------------------------------------------------------

    /** Sends a message to one chat; returns the error text to show, or null when it went out. */
    suspend fun send(sk: String, text: String, replyTo: String?): String? = try {
        Core.call("chat.send", buildJsonObject {
            put("sk", sk)
            put("text", text)
            if (replyTo != null) put("replyTo", replyTo)
        })
        null
    } catch (e: CoreError) {
        e.message ?: "Could not send"
    }

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
