package lol.agu.chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row as LayoutRow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coil3.compose.AsyncImage
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import lol.agu.chat.Core

private fun JsonObject.str(k: String): String = (this[k] as? JsonPrimitive)?.contentOrNull.orEmpty()
private fun JsonObject.flag(k: String): Boolean = (this[k] as? JsonPrimitive)?.contentOrNull == "true"

private val AmberText = Color(0xFFFCD34D)
private val OrangeText = Color(0xFFFFB86A)
private val RedText = Color(0xFFFCA5A5)
private val FaintText = Color(0xFF71717A)

private fun noteColor(tone: String) = when (tone) {
    "amber" -> AmberText
    "orange" -> OrangeText
    "red" -> RedText
    else -> FaintText
}

/** One line of the Twitch log tab. */
private class LogLine(val row: Row, val stamp: String, val note: String?)

/**
 * The user card: who they are (picture, account age, follow, subscription, a ban or timeout), the mod tools where the
 * account may use them, and tabs for their lines here, warnings, timeouts, bans and mod comments. What shows, and the
 * wording, is the core's (the desktop's card); this draws it as a sheet.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun UserCardSheet(vm: ChatViewModel, ctrl: ModController, req: CardRequest, onDismiss: () -> Unit, onLogin: () -> Unit) {
    val scope = rememberCoroutineScope()
    var info by remember(req) { mutableStateOf<JsonObject?>(null) }
    var failed by remember(req) { mutableStateOf(false) }
    var tab by remember(req) { mutableStateOf("messages") }
    var recent by remember(req) { mutableStateOf<List<Row>>(emptyList()) }
    val log = remember(req) { mutableStateListOf<LogLine>() }
    var logMore by remember(req) { mutableStateOf(false) }
    var logCursor by remember(req) { mutableStateOf<String?>(null) }
    var logBusy by remember(req) { mutableStateOf(false) }
    var logLoaded by remember(req) { mutableStateOf(false) }
    val comments = remember(req) { mutableStateListOf<JsonObject>() }
    var comment by remember(req) { mutableStateOf("") }
    var ignored by remember(req) { mutableStateOf<Boolean?>(null) }
    var signing by remember(req) { mutableStateOf(false) }
    val uri = LocalUriHandler.current

    fun parseRecent(o: JsonObject) = o.list("recent").mapNotNull { runCatching { Core.json.decodeFromJsonElement(Row.serializer(), it) }.getOrNull() }

    suspend fun load() {
        val o = vm.cardInfo(req.sk, req.user)
        if (o == null || o.flag("error")) { failed = true; return }
        info = o
        recent = withDefaultParse { parseRecent(o) }
        comments.clear(); comments.addAll(o.list("comments"))
        ignored = (o["ignore"] as? JsonPrimitive)?.contentOrNull?.let { it == "true" }
        if (tab !in o.list("tabs").map { it.str("id") }) tab = "messages"
    }

    suspend fun loadLog(reset: Boolean) {
        if (info == null || logBusy) return
        logBusy = true
        val o = vm.cardMessages(req.sk, req.user.id.orEmpty(), if (reset) null else logCursor)
        logBusy = false
        logLoaded = true
        if (o == null) return
        if (reset) log.clear()
        for (it in o.list("items")) {
            val parts = runCatching { Core.json.decodeFromJsonElement(ListSerializer(Part.serializer()), it["parts"] ?: JsonArray(emptyList())) }.getOrElse { emptyList() }
            val row = Row(key = it.str("key"), kind = "msg", parts = parts)
            log.add(LogLine(row, it.str("stamp"), it.str("note").takeIf { n -> n.isNotEmpty() && n != "null" }))
        }
        logMore = o.flag("more")
        logCursor = o.str("cursor").takeIf { it.isNotEmpty() && it != "null" }
    }

    LaunchedEffect(req) { load() }
    // an action that worked (a ban, a timeout...) changes what the card says
    var seenTick by remember(req) { mutableStateOf(ctrl.actionTick) }
    LaunchedEffect(ctrl.actionTick) {
        if (ctrl.actionTick != seenTick) {
            seenTick = ctrl.actionTick
            if (req.platform == "kick") delay(1500)
            load()
        }
    }
    LaunchedEffect(info, tab) {
        if (tab == "messages" && info?.flag("messagesWeb") == true && !logLoaded) loadLog(true)
    }

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = AguColors.Surface) {
        val i = info
        val shown = i ?: JsonObject(emptyMap())
        Column(Modifier.fillMaxWidth().navigationBarsPadding().verticalScroll(rememberScrollState()).padding(bottom = 24.dp)) {
            // ---- header
            val name = shown.str("name").ifEmpty { req.user.name ?: req.user.login ?: "" }
            val login = shown.str("login").ifEmpty { req.user.login.orEmpty() }
            val avatar = shown.str("avatar").ifEmpty { req.user.avatar.orEmpty() }
            val profile = shown.str("profile").takeIf { it.isNotEmpty() && it != "null" }
            LayoutRow(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Box(Modifier.size(48.dp).clip(CircleShape).background(AguColors.Surface2), contentAlignment = Alignment.Center) {
                    if (avatar.isNotEmpty()) AsyncImage(avatar, null, Modifier.size(48.dp), contentScale = ContentScale.Crop)
                    else Text(name.take(1).uppercase(), color = AguColors.Muted, fontSize = 20.sp, fontWeight = FontWeight.Bold)
                }
                Column(Modifier.weight(1f)) {
                    LayoutRow(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Box(Modifier.size(14.dp)) { PlatformMark(req.platform) }
                        Text(
                            name.ifEmpty { "unknown" }, color = AguColors.Text, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, maxLines = 1,
                            modifier = if (profile != null) Modifier.clickable { uri.openUri(profile) } else Modifier,
                        )
                    }
                    if (login.isNotEmpty() && login.lowercase() != name.lowercase()) Text("@$login", color = FaintText, fontSize = 12.sp)
                }
                Text("×", color = AguColors.Muted, fontSize = 26.sp, modifier = Modifier.clip(CircleShape).clickable(onClick = onDismiss).padding(horizontal = 12.dp))
            }

            if (failed) { Text("Couldn't load the card.", color = AguColors.Muted, modifier = Modifier.padding(16.dp)); return@Column }
            if (i == null) {
                LayoutRow(Modifier.padding(16.dp)) { Text("Loading…", color = AguColors.Muted, fontSize = 14.sp) }
                return@Column
            }

            // ---- badges: Twitch's own, else the ones on their newest line here
            val webBadges = i.list("badges").map { Badge(url = it.str("url").takeIf { u -> u.isNotEmpty() && u != "null" }, title = it.str("title")) }
            val badges = webBadges.ifEmpty { recent.firstOrNull()?.badges.orEmpty() }
            if (badges.isNotEmpty()) FlowRow(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                for (b in badges) {
                    val c = parseColor(b.color) ?: AguColors.Muted
                    if (!b.url.isNullOrEmpty()) AsyncImage(b.url, b.title, Modifier.size(22.dp).clip(RoundedCornerShape(3.dp)))
                    else Box(Modifier.height(22.dp).clip(RoundedCornerShape(4.dp)).background(c.copy(alpha = 0.2f)).padding(horizontal = 4.dp), contentAlignment = Alignment.Center) { Text(b.glyph ?: b.title.orEmpty().take(3), color = c, fontSize = 10.sp, fontWeight = FontWeight.Bold) }
                }
            }

            // ---- account info, ban state, notes
            Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                for (l in i.list("info")) Text(l.str("text"), color = AguColors.Muted, fontSize = 13.sp)
                for (c in i.list("chips")) {
                    val col = if (c.str("tone") == "red") RedText else AmberText
                    Text(c.str("text"), color = col, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.clip(CircleShape).border(1.dp, col.copy(alpha = 0.3f), CircleShape).background(col.copy(alpha = 0.1f)).padding(horizontal = 10.dp, vertical = 3.dp))
                }
                for (n in i.list("notes")) Text(n.str("text"), color = noteColor(n.str("tone")), fontSize = 12.sp)
            }

            // ---- mod tools
            val mod = i["mod"] as? JsonObject
            val target = (i["target"] as? JsonObject)
            fun targetOf(): ModTarget =
                if (mod?.str("kind") == "twitch" || target == null) ModTarget(uid = i.str("id"), login = i.str("login"), name = i.str("name"))
                else ModTarget(id = target.str("id"), uid = target.str("uid"), login = target.str("login"), name = target.str("name"), menu = target.str("menu").takeIf { it.isNotEmpty() && it != "null" })
            if (mod != null) Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                val steps = (mod["ladder"] as? JsonArray)?.mapNotNull { l -> (l as? JsonArray)?.let { a -> a[0].jsonPrimitive.content to (a[1].jsonPrimitive.content.toIntOrNull() ?: 0) } }.orEmpty()
                if (steps.isNotEmpty()) {
                    Text("Timeout", color = FaintText, fontSize = 11.sp, fontWeight = FontWeight.SemiBold)
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        for ((label, secs) in steps) Text(
                            label, color = AguColors.Muted, fontSize = 13.sp,
                            modifier = Modifier.clip(RoundedCornerShape(6.dp)).border(1.dp, AguColors.Surface2, RoundedCornerShape(6.dp)).clickable { ctrl.act(req.sk, "timeout", targetOf(), seconds = secs) }.padding(horizontal = 12.dp, vertical = 7.dp),
                        )
                    }
                }
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    when (mod.str("kind")) {
                        "twitch" -> {
                            if (mod.flag("warn")) Pill("Warn", AguColors.Text) { ctrl.warn(req.sk, targetOf()) }
                            mod.str("unban").takeIf { it.isNotEmpty() && it != "null" }?.let { l -> Pill(l, AguColors.Text) { ctrl.act(req.sk, "unban", targetOf()) } }
                            if (mod.flag("ban")) Pill("Ban", AguColors.Danger) { ctrl.ban(req.sk, "twitch", targetOf()) }
                        }
                        "rumble" -> {
                            Pill("Timeout 5m", AguColors.Text) { ctrl.act(req.sk, "timeout", targetOf(), seconds = 300) }
                            Pill("Ban", AguColors.Danger) { ctrl.ban(req.sk, "rumble", targetOf()) }
                        }
                        "kick" -> {
                            mod.str("unban").takeIf { it.isNotEmpty() && it != "null" }?.let { l -> Pill(l, AguColors.Text) { ctrl.act(req.sk, "unban", targetOf()) } }
                            if (mod.flag("ban")) Pill("Ban", AguColors.Danger) { ctrl.ban(req.sk, "kick", targetOf()) }
                        }
                    }
                    ignored?.let { ig -> Pill(if (ig) "Unignore user" else "Ignore user", AguColors.Text) { scope.launch { ctrl.toast(vm.cardIgnore(req.platform, login.ifEmpty { name }, !ig)); ignored = !ig } } }
                }
            } else ignored?.let { ig ->
                LayoutRow(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
                    Pill(if (ig) "Unignore user" else "Ignore user", AguColors.Text) { scope.launch { ctrl.toast(vm.cardIgnore(req.platform, login.ifEmpty { name }, !ig)); ignored = !ig } }
                }
            }

            // ---- a moderator without a twitch.tv session
            if (i.flag("signin")) LayoutRow(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("Log in to Twitch again to see warnings, timeouts, bans and mod comments", color = FaintText, fontSize = 12.sp, modifier = Modifier.weight(1f))
                Pill(if (signing) "Waiting…" else "Log in to Twitch", AguColors.Text, enabled = !signing) {
                    signing = true
                    scope.launch { vm.login("twitch"); signing = false; onLogin(); load() }
                }
            }

            // ---- tabs
            val tabs = i.list("tabs")
            LayoutRow(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 8.dp)) {
                for (t in tabs) {
                    val on = tab == t.str("id")
                    LayoutRow(
                        Modifier.clickable { tab = t.str("id") }.padding(horizontal = 8.dp, vertical = 10.dp),
                        horizontalArrangement = Arrangement.spacedBy(5.dp),
                    ) {
                        Text(t.str("label"), color = if (on) AguColors.Text else AguColors.Muted, fontSize = 13.sp, fontWeight = if (on) FontWeight.SemiBold else FontWeight.Normal)
                        Text(t.str("count"), color = FaintText, fontSize = 13.sp)
                    }
                }
            }
            Box(Modifier.fillMaxWidth().height(1.dp).background(AguColors.Surface2))

            when (tab) {
                "messages" -> if (i.flag("messagesWeb")) {
                    if (!logLoaded) Text("Loading…", color = FaintText, fontSize = 13.sp, modifier = Modifier.padding(16.dp))
                    else if (log.isEmpty()) Text("No messages.", color = FaintText, fontSize = 13.sp, modifier = Modifier.padding(16.dp))
                    else Column(Modifier.padding(vertical = 6.dp)) {
                        for (l in log) Column {
                            LayoutRow(Modifier.padding(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                Text(l.stamp, color = FaintText, fontSize = 11.sp, modifier = Modifier.padding(top = 4.dp))
                                Box(Modifier.weight(1f)) { MessageRow(l.row, onLongPress = {}) }
                            }
                            l.note?.let { Text(it, color = OrangeText, fontSize = 11.sp, modifier = Modifier.padding(start = 12.dp, bottom = 2.dp)) }
                        }
                        if (logMore) LayoutRow(Modifier.padding(12.dp)) { Pill(if (logBusy) "Loading…" else "Load more", AguColors.Text, enabled = !logBusy) { scope.launch { loadLog(false) } } }
                    }
                } else if (recent.isEmpty()) Text("No recent messages.", color = FaintText, fontSize = 13.sp, modifier = Modifier.padding(16.dp))
                else Column(Modifier.padding(vertical = 6.dp)) {
                    val canDelete = i.flag("canDelete")
                    for (r in recent) LayoutRow(Modifier.fillMaxWidth(), verticalAlignment = Alignment.Top) {
                        Box(Modifier.weight(1f)) { MessageRow(r, onLongPress = {}) }
                        val id = r.id?.content
                        if (canDelete && !r.deleted && id != null) Box(Modifier.size(36.dp).clip(CircleShape).clickable { ctrl.act(req.sk, "delete", ModTarget(id = id)) }, contentAlignment = Alignment.Center) { ModIcon("trash", FaintText, 18) }
                    }
                }
                "warnings", "timeouts", "bans" -> {
                    val items = i.list(tab)
                    if (items.isEmpty()) Text(when (tab) { "warnings" -> "No warnings."; "timeouts" -> "No timeouts."; else -> "No bans." }, color = FaintText, fontSize = 13.sp, modifier = Modifier.padding(16.dp))
                    else Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        for (e in items) LayoutRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            val col = when (e.str("kind")) { "TIMEOUT" -> AmberText; "BAN" -> RedText; "WARN" -> OrangeText; else -> AguColors.Muted }
                            Text(e.str("text"), color = col, fontSize = 13.sp, modifier = Modifier.weight(1f))
                            Text(e.str("stamp"), color = FaintText, fontSize = 11.sp)
                        }
                    }
                }
                "comments" -> Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    LayoutRow(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        FieldBox(comment, { comment = it.take(500) }, "Add a comment", Modifier.weight(1f))
                        Pill("Add", AguColors.Accent, enabled = comment.isNotBlank()) {
                            val text = comment.trim()
                            scope.launch {
                                val o = vm.cardComment(req.sk, i.str("commentTarget"), text)
                                if (o?.flag("ok") == true) { (o["comment"] as? JsonObject)?.let { comments.add(0, it) }; comment = "" } else ctrl.toast(o?.str("toast") ?: "Couldn't add the comment")
                            }
                        }
                    }
                    if (comments.isEmpty()) Text("No comments yet.", color = FaintText, fontSize = 13.sp)
                    for (c in comments) LayoutRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Column(Modifier.weight(1f)) {
                            Text(c.str("text"), color = AguColors.Text, fontSize = 14.sp)
                            Text("${c.str("by")} - ${c.str("stamp")}", color = FaintText, fontSize = 11.sp)
                        }
                        if (c.flag("mine")) Box(Modifier.size(36.dp).clip(CircleShape).clickable {
                            scope.launch {
                                val o = vm.cardCommentDelete(req.sk, c.str("id"))
                                if (o?.flag("ok") == true) comments.remove(c) else ctrl.toast(o?.str("toast"))
                            }
                        }, contentAlignment = Alignment.Center) { ModIcon("trash", FaintText, 18) }
                    }
                    Text("Shared with this channel's moderators on Twitch.", color = FaintText, fontSize = 11.sp)
                }
            }
        }
    }
}

private suspend fun <T> withDefaultParse(block: () -> T): T = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) { block() }
