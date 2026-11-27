package lol.agu.chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.FilterChipDefaults
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.put

private fun JsonObject.str(k: String): String = (this[k] as? JsonPrimitive)?.contentOrNull.orEmpty()
private fun JsonObject.flag(k: String): Boolean = (this[k] as? JsonPrimitive)?.contentOrNull == "true"
private fun JsonObject.num(k: String): Int = str(k).toIntOrNull() ?: 0
private fun options(a: kotlinx.serialization.json.JsonElement?): List<Pair<Int, String>> =
    (a as? JsonArray)?.mapNotNull { (it as? JsonArray)?.let { p -> (p[0] as JsonPrimitive).content.toInt() to (p[1] as JsonPrimitive).content } }.orEmpty()

private val AmberText = Color(0xFFFCD34D)
private val RedText = Color(0xFFFCA5A5)

/**
 * The channel modes sheet (the shield button): Shield Mode, subscribers-only, emote-only, unique chat, followers-only,
 * slow mode and the blocked terms, for the Twitch channels of the tab the account may change. The core makes the calls
 * and words the errors, as the desktop's panel does.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun ModesSheet(vm: ChatViewModel, channels: List<ChannelRef>, onDismiss: () -> Unit) {
    val scope = rememberCoroutineScope()
    var pick by remember { mutableStateOf(channels.first().sk()) }
    val sk = if (channels.any { it.sk() == pick }) pick else channels.first().sk()
    var data by remember(sk) { mutableStateOf<JsonObject?>(null) }
    var shield by remember(sk) { mutableStateOf<Boolean?>(null) }
    var busy by remember(sk) { mutableStateOf(false) }
    var error by remember(sk) { mutableStateOf("") }
    var termsOpen by remember(sk) { mutableStateOf(false) }
    var termsLoading by remember(sk) { mutableStateOf(false) }
    val terms = remember(sk) { mutableStateListOf<JsonObject>() }
    var termText by remember(sk) { mutableStateOf("") }
    var termError by remember(sk) { mutableStateOf("") }

    LaunchedEffect(sk) {
        val o = vm.modesGet(sk, fresh = true)
        data = o
        shield = (o?.get("shield") as? JsonPrimitive)?.contentOrNull?.let { it == "true" }
    }

    fun loadTerms() {
        termsLoading = true
        scope.launch {
            val o = vm.termsList(sk)
            termsLoading = false
            terms.clear()
            (o?.get("terms") as? JsonArray)?.forEach { (it as? JsonObject)?.let(terms::add) }
        }
    }

    fun patch(body: JsonObject) {
        busy = true; error = ""
        scope.launch {
            val o = vm.modesPatch(sk, body)
            busy = false
            if (o == null || o["error"] != null) error = o?.str("error").orEmpty().ifEmpty { "update failed" }
            else data = JsonObject(o + ("shield" to (shield?.let { JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull)))
        }
    }

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = AguColors.Surface) {
        Column(Modifier.fillMaxWidth().navigationBarsPadding().verticalScroll(rememberScrollState()).padding(horizontal = 20.dp).padding(bottom = 24.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
            if (channels.size > 1) Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (c in channels) FilterChip(
                    selected = c.sk() == sk, onClick = { pick = c.sk() }, label = { Text(c.channel) },
                    colors = FilterChipDefaults.filterChipColors(selectedContainerColor = AguColors.Surface2, selectedLabelColor = AguColors.Text),
                )
            } else Text("${channels.first().channel} channel modes", color = AguColors.Text, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)

            val d = data
            val caps = d?.get("caps") as? JsonObject
            val canModes = caps?.flag("modes") == true
            val canShield = caps?.flag("shield") == true
            val canTerms = caps?.flag("terms") == true
            val off = !canModes || busy

            ToggleRow("Shield Mode", shield == true, enabled = canShield && !busy && d != null && shield != null) {
                busy = true; error = ""
                scope.launch {
                    val o = vm.modesShield(sk, it)
                    busy = false
                    if (o?.get("error") != null) error = o.str("error").ifEmpty { "update failed" } else shield = o?.flag("shield") ?: it
                }
            }
            if (d != null && !canModes) Text("Log in again to enable chat mode controls.", color = AmberText, fontSize = 13.sp)
            val modes = d?.get("modes") as? JsonObject
            if (modes != null) {
                ToggleRow("Subscribers-Only", modes.flag("subscriber"), !off) { patch(buildJsonObject { put("subscriber_mode", it) }) }
                ToggleRow("Emote-Only", modes.flag("emote"), !off) { patch(buildJsonObject { put("emote_mode", it) }) }
                ToggleRow("Unique (R9K)", modes.flag("unique"), !off) { patch(buildJsonObject { put("unique_chat_mode", it) }) }
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("Followers-Only", color = AguColors.Text, fontSize = 15.sp)
                    val cur = d.num("followerCur")
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        for ((v, label) in options(d["followerOptions"])) Choice(label, v == cur, !off) {
                            patch(if (v < 0) buildJsonObject { put("follower_mode", false) } else buildJsonObject { put("follower_mode", true); put("follower_mode_duration", v) })
                        }
                    }
                }
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("Slow Mode", color = AguColors.Text, fontSize = 15.sp)
                    val cur = d.num("slowCur")
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        for ((v, label) in options(d["slowOptions"])) Choice(label, v == cur, !off) {
                            patch(if (v <= 0) buildJsonObject { put("slow_mode", false) } else buildJsonObject { put("slow_mode", true); put("slow_mode_wait_time", v) })
                        }
                    }
                }
            } else if (d == null) Text("Loading…", color = AguColors.Muted, fontSize = 14.sp)
            if (error.isNotEmpty()) Text(error, color = RedText, fontSize = 13.sp)

            // ---- blocked terms
            Row(Modifier.fillMaxWidth().clickable { termsOpen = !termsOpen; if (termsOpen && canTerms) loadTerms() }.padding(top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("BLOCKED TERMS", color = Color(0xFF71717A), fontSize = 11.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                Text(if (termsOpen) "▾" else "▸", color = AguColors.Muted)
            }
            if (termsOpen) Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                if (!canTerms) Text("Log in again to manage blocked terms.", color = AmberText, fontSize = 13.sp)
                if (termsLoading) Text("Loading…", color = AguColors.Muted, fontSize = 13.sp)
                else if (terms.isEmpty()) Text("No blocked terms.", color = Color(0xFF71717A), fontSize = 13.sp)
                for (t in terms) Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(t.str("text"), color = AguColors.Text, fontSize = 15.sp, maxLines = 1, modifier = Modifier.weight(1f))
                    Text("×", color = AguColors.Muted, fontSize = 22.sp, modifier = Modifier.clip(RoundedCornerShape(8.dp)).clickable(enabled = canTerms && !busy) {
                        busy = true; termError = ""
                        scope.launch {
                            val o = vm.termsRemove(sk, t.str("id"))
                            busy = false
                            if (o?.get("error") != null) termError = o.str("error") else terms.remove(t)
                        }
                    }.padding(horizontal = 12.dp))
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    FieldBox(termText, { termText = it.take(100) }, "Add a blocked term", Modifier.weight(1f))
                    Pill("Add", AguColors.Accent, enabled = canTerms && !busy && termText.isNotBlank()) {
                        val text = termText.trim()
                        busy = true; termError = ""
                        scope.launch {
                            val o = vm.termsAdd(sk, text)
                            busy = false
                            if (o?.get("error") != null) termError = o.str("error") else { termText = ""; loadTerms() }
                        }
                    }
                }
                if (termError.isNotEmpty()) Text(termError, color = RedText, fontSize = 13.sp)
                Text("Permitted terms aren't exposed by Twitch's API.", color = Color(0xFF71717A), fontSize = 12.sp)
            }
        }
    }
}

@Composable
private fun ToggleRow(label: String, on: Boolean, enabled: Boolean, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(label, color = if (enabled) AguColors.Text else AguColors.Muted, fontSize = 15.sp, modifier = Modifier.weight(1f))
        Switch(on, onChange, enabled = enabled, colors = SwitchDefaults.colors(checkedTrackColor = AguColors.Accent, checkedThumbColor = Color.White))
    }
}

@Composable
private fun Choice(label: String, selected: Boolean, enabled: Boolean, onClick: () -> Unit) {
    Text(
        label, color = if (selected) Color.White else if (enabled) AguColors.Text else AguColors.Muted, fontSize = 13.sp,
        modifier = Modifier.clip(RoundedCornerShape(8.dp)).background(if (selected) AguColors.Accent else Color.Transparent)
            .border(1.dp, if (selected) AguColors.Accent else AguColors.Surface2, RoundedCornerShape(8.dp)).clickable(enabled = enabled && !selected, onClick = onClick).padding(horizontal = 12.dp, vertical = 7.dp),
    )
}
