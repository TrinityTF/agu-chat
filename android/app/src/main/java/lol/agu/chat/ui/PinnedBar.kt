package lol.agu.chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
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
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay

/**
 * The pinned messages at the top of a chat: Rumble's (a bar with the pin icon, who said it and the line, and an Unpin
 * button for a moderator) and Twitch's (a card: "Pinned by" with the pinner's badges, the message, opened out to more).
 * The core sends the lines already drawn (`pins` of the rows event).
 */
@Composable
fun PinnedBars(tab: Tab, vm: ChatViewModel, ctrl: ModController?) {
    val channels = tab.channels.filter { !it.isWhisper() }
    val twitch = vm.twitchPins
    val pins = vm.pins
    val shown = channels.filter { twitch.containsKey(it.sk()) || pins.containsKey(it.sk()) }
    if (shown.isEmpty()) return
    val multi = channels.count { it.platform == "twitch" } > 1
    Column(Modifier.fillMaxWidth().padding(horizontal = 6.dp).padding(top = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        for (ch in shown) {
            val t = twitch[ch.sk()]
            if (t != null) {
                TwitchPinCard(vm, ch, t, multi, ctrl)
                continue
            }
            val p = pins.getValue(ch.sk())
            Row(
                Modifier.fillMaxWidth().clip(RoundedCornerShape(4.dp)).background(AguColors.Surface).border(1.dp, AguColors.Surface2, RoundedCornerShape(4.dp)).padding(horizontal = 10.dp, vertical = 6.dp),
                verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Column(Modifier.weight(1f).heightIn(max = 120.dp).verticalScroll(rememberScrollState())) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        ModIcon("pin", AguColors.Muted, 12)
                        Text("Pinned", color = AguColors.Muted, fontSize = 11.sp)
                    }
                    CompositionLocalProvider(LocalModMode provides false) { MessageRow(p.row, onLongPress = {}) }
                }
                if (p.canMod && ctrl != null) {
                    val t = p.row.mod?.t ?: ModTarget(id = p.row.id?.content.orEmpty(), login = p.row.user?.login.orEmpty(), name = p.row.user?.name.orEmpty())
                    Text("Unpin", color = AguColors.Muted, fontSize = 12.sp, modifier = Modifier.clip(RoundedCornerShape(4.dp)).clickable { ctrl.pin(ch.sk(), t, unpin = true) }.padding(horizontal = 8.dp, vertical = 4.dp))
                }
            }
        }
    }
}

/**
 * One Twitch pin, as the desktop draws it: the pin icon, the channel (when several are open), "Pinned by" with the
 * pinner's badges and name, and the message cut to its first line. Opened out it also says what the message answered,
 * who sent it when, and how long is left. The menu: Unpin and Unpin After for a moderator, Hide for Yourself for anyone.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun TwitchPinCard(vm: ChatViewModel, ch: ChannelRef, p: TwitchPin, multi: Boolean, ctrl: ModController?) {
    val d = LocalDisplay.current
    val open = vm.pinsOpen[p.id] == true
    var menu by remember { mutableStateOf(false) }
    var left by remember(p.id) { mutableStateOf<String?>(null) }
    var over by remember(p.id) { mutableStateOf(false) }
    // what is left on the pin; a pin that ended goes away (the core says when)
    LaunchedEffect(p.id, p.endsAtMs) {
        while (true) {
            val (text, gone) = vm.pinLeft(p.endsAtMs)
            left = text
            over = gone
            if (p.endsAtMs == null) break
            delay(5_000)
        }
    }
    if (over) return
    val row = p.row
    val ctx = LocalContext.current
    val ink = parseColor(row.user?.color) ?: AguColors.Text
    Row(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(4.dp)).background(AguColors.Surface).border(1.dp, AguColors.Surface2, RoundedCornerShape(4.dp)).padding(start = 2.dp, end = 4.dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Column(Modifier.weight(1f)) {
            Row(Modifier.padding(start = 8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                ModIcon("pin", AguColors.Muted, 12)
                if (multi) Text("#${ch.channel}", color = AguColors.Muted, fontSize = 10.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.clip(RoundedCornerShape(4.dp)).background(AguColors.Text.copy(alpha = 0.1f)).padding(horizontal = 4.dp))
                BadgedName(if (p.byName != null) "Pinned by " else "Pinned", p.byBadges, p.byName, AguColors.Text, "", 11f)
            }
            if (open) row.reply?.let { r ->
                Text("Replying to @${r.user.orEmpty()}: ${r.text.orEmpty()}", color = AguColors.Muted, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(start = 8.dp, top = 4.dp))
            }
            // the message alone: no name, badges, time or reply line of its own
            val body = remember(row) { row.copy(user = null, badges = emptyList(), reply = null, mod = null, highlight = null, highlighted = false, mention = false, firstMsg = false, returning = false, link = null, action = false) }
            Box(Modifier.fillMaxWidth().let { if (open) it else it.heightIn(max = (d.bodySp * 1.75f).dp).clipToBounds() }) {
                CompositionLocalProvider(LocalModMode provides false, LocalMod provides null, LocalDisplay provides d.copy(timestamps = false)) { MessageRow(body, onLongPress = {}) }
            }
            if (open) {
                val sent = " sent at ${formatTime(row.ts, false, is24Hour(ctx))}" + (left?.let { " · unpins in $it" } ?: "")
                BadgedName("", row.badges, row.user?.name ?: row.user?.login, ink, sent, 12f, Modifier.padding(start = 8.dp, top = 2.dp))
            }
        }
        PinBtn("⋯") { menu = true }
        PinBtn(if (open) "⌃" else "⌄") { vm.pinsOpen[p.id] = !open }
    }
    if (menu) ModalBottomSheet(onDismissRequest = { menu = false }, containerColor = AguColors.Surface) {
        Column(Modifier.navigationBarsPadding().padding(bottom = 12.dp)) {
            if (p.canMod && ctrl != null) {
                SheetRow("Unpin this Message") { menu = false; ctrl.unpinTwitch(ch.sk(), p.id) }
                SheetRow("Unpin After") { menu = false; ctrl.unpinAfter(ch.sk(), p.id, p.endsAtMs) }
            }
            SheetRow("Hide for Yourself") { menu = false; vm.hidePin(p.id) }
        }
    }
}

@Composable
private fun PinBtn(glyph: String, onClick: () -> Unit) {
    Box(Modifier.size(32.dp).clip(CircleShape).clickable(onClick = onClick), contentAlignment = Alignment.Center) {
        Text(glyph, color = AguColors.Muted, fontSize = 18.sp)
    }
}
