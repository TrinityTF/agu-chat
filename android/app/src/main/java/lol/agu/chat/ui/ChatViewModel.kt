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

/** State holder for the whole app: tabs, per-chat rows, connection states and accounts. Main-thread state only. */
class ChatViewModel(app: Application) : AndroidViewModel(app) {
    val tabs = mutableStateListOf<Tab>()
    val conns = mutableStateMapOf<String, Conn>()
    var accounts by mutableStateOf<Map<String, AccountState>>(emptyMap())
        private set
    var appVersion by mutableStateOf("")
        private set
    /** The sign-in WebView the core asked for (YouTube, X, Twitch), or null. */
    var loginRequest by mutableStateOf<LoginRequest?>(null)
        private set
    /** Errors of a login call that failed on the spot (the core's own errors come with the account state). */
    val loginErrors = mutableStateMapOf<String, String>()
    /** Platforms with a login call in flight (Rumble's password or code is being checked). */
    val loginBusy = mutableStateMapOf<String, Boolean>()

    private val rowLists = HashMap<String, SnapshotStateList<Row>>()
    private val rowIndex = HashMap<String, HashMap<String, Int>>() // sk -> row key -> position

    fun rowsFor(sk: String): SnapshotStateList<Row> = rowLists.getOrPut(sk) { mutableStateListOf() }

    init {
        // Collect the core's events first so nothing is missed while the saved tabs reopen.
        viewModelScope.launch(Dispatchers.Default) {
            Core.events.collect { e ->
                when (e.kind) {
                    "rows" -> onRows(e.data)
                    "conn" -> onConn(e.data)
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
            restoreTabs()
            refreshAccounts()
            runCatching { Core.call("app.info").jsonObject["version"]?.jsonPrimitive?.contentOrNull }
                .getOrNull()?.let { appVersion = it }
        }
    }

    // ---- events --------------------------------------------------------------------------------

    private suspend fun onRows(data: JsonElement) {
        val o = data as? JsonObject ?: return
        val sk = o["sk"]?.jsonPrimitive?.contentOrNull ?: return
        val reset = o["reset"]?.jsonPrimitive?.contentOrNull == "true"
        // Decode off the main thread; a malformed row is skipped rather than dropping the batch.
        val upsert = (o["upsert"] as? JsonArray)?.mapNotNull {
            runCatching { Core.json.decodeFromJsonElement(Row.serializer(), it) }.getOrNull()
        } ?: emptyList()
        val remove = (o["remove"] as? JsonArray)?.mapNotNull { it.jsonPrimitive.contentOrNull } ?: emptyList()
        withContext(Dispatchers.Main) { applyRows(sk, reset, upsert, remove) }
    }

    private fun applyRows(sk: String, reset: Boolean, upsert: List<Row>, remove: List<String>) {
        val list = rowsFor(sk)
        val index = rowIndex.getOrPut(sk) { HashMap() }
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

    private fun parseAccounts(data: JsonElement): Map<String, AccountState>? {
        val o = data as? JsonObject ?: return null
        return o.mapNotNull { (k, v) ->
            runCatching { k to Core.json.decodeFromJsonElement(AccountState.serializer(), v) }.getOrNull()
        }.toMap()
    }

    // ---- tabs ----------------------------------------------------------------------------------

    private suspend fun restoreTabs() {
        val saved = runCatching {
            Core.json.decodeFromJsonElement(kotlinx.serialization.builtins.ListSerializer(SavedTab.serializer()), Core.call("tabs.get"))
        }.getOrElse { emptyList() }
        // Show the tabs immediately with a guessed source key, then open each chat.
        tabs.addAll(saved.map { Tab(it.platform, it.channel, "${it.platform}:${it.channel.lowercase()}") })
        for (s in saved) {
            viewModelScope.launch {
                val sk = openChat(s.platform, s.channel) ?: return@launch
                val i = tabs.indexOfFirst { it.platform == s.platform && it.channel == s.channel }
                if (i >= 0) tabs[i] = tabs[i].copy(sk = sk)
            }
        }
    }

    private suspend fun openChat(platform: String, channel: String): String? = runCatching {
        Core.call("chat.open", buildJsonObject { put("platform", platform); put("channel", channel) })
            .jsonObject["sk"]?.jsonPrimitive?.contentOrNull
    }.getOrNull()

    /** Opens a chat and adds its tab; returns an error message to show, or null on success. */
    suspend fun addTab(platform: String, channelRaw: String): String? {
        val channel = channelRaw.trim().trimStart('#', '@')
        if (channel.isEmpty()) return "Enter a channel name"
        val existing = tabs.firstOrNull { it.platform == platform && it.channel.equals(channel, true) }
        if (existing != null) return null
        return try {
            val sk = Core.call("chat.open", buildJsonObject { put("platform", platform); put("channel", channel) })
                .jsonObject["sk"]?.jsonPrimitive?.contentOrNull ?: "$platform:${channel.lowercase()}"
            tabs.add(Tab(platform, channel, sk))
            saveTabs()
            null
        } catch (e: CoreError) {
            e.message ?: "Could not open the chat"
        }
    }

    fun closeTab(tab: Tab) {
        tabs.remove(tab)
        viewModelScope.launch {
            runCatching { Core.call("chat.close", buildJsonObject { put("sk", tab.sk) }) }
            rowLists.remove(tab.sk)
            rowIndex.remove(tab.sk)
            conns.remove(tab.sk)
            saveTabs()
        }
    }

    private suspend fun saveTabs() {
        val arr = JsonArray(tabs.map { buildJsonObject { put("platform", it.platform); put("channel", it.channel) } })
        runCatching { Core.call("tabs.set", buildJsonObject { put("tabs", arr) }) }
    }

    // ---- sending -------------------------------------------------------------------------------

    /** Sends a message; returns the error text to show, or null when it went out. */
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
