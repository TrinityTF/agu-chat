package lol.agu.chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.foundation.Canvas
import coil3.compose.AsyncImage
import kotlinx.coroutines.delay

private val BarBg = Color(0xFF26262C)
private val Raised = Color(0xFF1F1F23)
private val Line = Color(0xFF3A3A44)
private val PurpleLight = Color(0xFFBF94FF)
private val AmberLight = Color(0xFFFBBF24)

/** "4:40": minutes and seconds left, never below 0:00 (the core's `time_left`). */
internal fun timeLeft(expiresMs: Long, nowMs: Long): String {
    val s = Math.ceil((expiresMs - nowMs) / 1000.0).coerceAtLeast(0.0).toLong()
    return "${s / 60}:${(s % 60).toString().padStart(2, '0')}"
}

private fun hexColor(h: String): Color = runCatching { Color(android.graphics.Color.parseColor(h)) }.getOrDefault(Color(0xFF9147FF))

/**
 * The hype train bars at the top of a chat, like twitch.tv's: collapsed a progress bar ("Hype Train", "Lvl N", time
 * left, percent of the level); a tap opens what is left to finish the level, the level's emotes, a few lines of info and
 * the top contributors. The details hang over the messages instead of pushing them away, so they are [HypeDetails],
 * drawn by the chat on top of its list.
 */
@Composable
fun HypeBars(tab: Tab, trains: Map<String, HypeTrain>, open: Set<String>, onToggle: (String) -> Unit) {
    val shown = tab.channels.filter { !it.isWhisper() && trains.containsKey(it.sk()) }
    if (shown.isEmpty()) return
    // the countdown ticks once a second
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) { while (true) { delay(1000); now = System.currentTimeMillis() } }
    val multi = tab.channels.count { it.platform == "twitch" && !it.isWhisper() } > 1
    Column(Modifier.fillMaxWidth().padding(horizontal = 6.dp).padding(top = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        for (ch in shown) {
            val t = trains.getValue(ch.sk())
            if (t.expiresMs != null && now > t.expiresMs + 3000) continue
            val isOpen = ch.sk() in open
            val shape = if (isOpen) RoundedCornerShape(topStart = 6.dp, topEnd = 6.dp) else RoundedCornerShape(6.dp)
            Box(Modifier.fillMaxWidth().height(52.dp).clip(shape).background(BarBg).clickable { onToggle(ch.sk()) }) {
                Box(Modifier.fillMaxHeight().fillMaxWidth(t.percent.coerceIn(0, 100) / 100f).background(hexColor(t.fill)))
                Row(Modifier.fillMaxWidth().fillMaxHeight().padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(if (multi) "Hype Train · #${ch.channel}" else "Hype Train", color = Color.White, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        Text("Lvl ${t.level}", color = Color.White, fontSize = 17.sp)
                    }
                    Column(horizontalAlignment = Alignment.End) {
                        Text(t.expiresMs?.let { timeLeft(it, now) }.orEmpty(), color = Color.White, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                        Text("${t.percent}%", color = Color.White, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
                    }
                    Canvas(Modifier.padding(start = 8.dp).size(16.dp)) {
                        val w = size.width
                        val p = Path().apply {
                            if (isOpen) { moveTo(w * 0.2f, w * 0.65f); lineTo(w * 0.5f, w * 0.35f); lineTo(w * 0.8f, w * 0.65f) }
                            else { moveTo(w * 0.2f, w * 0.35f); lineTo(w * 0.5f, w * 0.65f); lineTo(w * 0.8f, w * 0.35f) }
                        }
                        drawPath(p, Color.White, style = Stroke(width = w * 0.12f, cap = StrokeCap.Round, join = StrokeJoin.Round))
                    }
                }
            }
        }
    }
}

/** The open bars' details, over the top of the message list; scrolls when tall. */
@Composable
fun HypeDetails(tab: Tab, trains: Map<String, HypeTrain>, open: Set<String>, barsHeightDp: Int, modifier: Modifier = Modifier) {
    val list = tab.channels.filter { it.sk() in open && trains.containsKey(it.sk()) }
    if (list.isEmpty()) return
    Column(
        modifier.fillMaxWidth().padding(horizontal = 6.dp).padding(top = barsHeightDp.dp).heightIn(max = 380.dp)
            .clip(RoundedCornerShape(bottomStart = 6.dp, bottomEnd = 6.dp)).background(Raised).border(1.dp, Line, RoundedCornerShape(bottomStart = 6.dp, bottomEnd = 6.dp))
            .verticalScroll(rememberScrollState()).padding(10.dp),
    ) {
        for (ch in list) Details(trains.getValue(ch.sk()))
    }
}

@Composable
private fun Heading(text: String) {
    Text(text, color = AguColors.Text, fontSize = 13.sp, fontWeight = FontWeight.Bold, modifier = Modifier.padding(top = 10.dp, bottom = 6.dp))
}

@Composable
private fun Details(t: HypeTrain) {
    // a long press on an unlockable emote opens its popup, like the desktop's hover tip
    var tip by remember { androidx.compose.runtime.mutableStateOf<Tip?>(null) }
    tip?.let { TipDialog(it, LocalDisplay.current) { tip = null } }
    Column(Modifier.fillMaxWidth()) {
        // what is left of the level
        Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(6.dp)).background(Color.White.copy(alpha = 0.06f)).padding(horizontal = 10.dp, vertical = 8.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(t.bits, color = AguColors.Text, fontSize = 13.sp, fontWeight = FontWeight.Bold)
                Text("  or  ", color = AguColors.Muted, fontSize = 13.sp)
                Text(t.subs, color = AguColors.Text, fontSize = 13.sp, fontWeight = FontWeight.Bold)
            }
            Text("remaining to complete level ${t.level}", color = AguColors.Muted, fontSize = 12.sp)
        }
        if (t.emotes.isNotEmpty()) {
            Heading("Unlockable Emotes")
            Row(Modifier.fillMaxWidth().clip(RoundedCornerShape(6.dp)).background(Color.White.copy(alpha = 0.04f)).padding(horizontal = 8.dp, vertical = 4.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (e in t.emotes.take(8)) AsyncImage(model = e.url, contentDescription = e.token, contentScale = ContentScale.Fit, modifier = Modifier.size(32.dp).pointerInput(e.url) { detectTapGestures(onLongPress = { tip = Tip(e.token, "Hype Train emote", e.url) }) })
            }
        }
        if (t.info.isNotEmpty()) {
            Heading("Info")
            for (line in t.info) Row(
                Modifier.fillMaxWidth().padding(bottom = 4.dp).clip(RoundedCornerShape(6.dp)).background(Color.White.copy(alpha = 0.04f)).padding(horizontal = 10.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Box(Modifier.size(8.dp).clip(CircleShape).background(AmberLight))
                Text(line, color = AguColors.Text, fontSize = 12.sp, modifier = Modifier.weight(1f))
            }
        }
        if (t.conductors.isNotEmpty()) {
            Heading("Top Contributors")
            for (c in t.conductors) Row(Modifier.fillMaxWidth().padding(bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                if (c.avatar.isNotEmpty()) AsyncImage(model = c.avatar, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.size(24.dp).clip(CircleShape).background(AguColors.Surface2))
                Text(c.name, color = AguColors.Text, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(start = if (c.avatar.isNotEmpty()) 8.dp else 0.dp))
                Text("  ${c.what}", color = AguColors.Muted, fontSize = 13.sp)
            }
        }
    }
}
