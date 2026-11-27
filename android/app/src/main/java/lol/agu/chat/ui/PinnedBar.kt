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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Rumble's pinned message at the top of its chat: a bar with the pin icon, who said it and the line (the chat's own
 * emotes drawn), and an Unpin button for a moderator. The core sends the line already drawn (`pins` of the rows event).
 */
@Composable
fun PinnedBars(tab: Tab, pins: Map<String, RumblePin>, ctrl: ModController?) {
    val shown = tab.channels.filter { !it.isWhisper() && pins.containsKey(it.sk()) }
    if (shown.isEmpty()) return
    Column(Modifier.fillMaxWidth().padding(horizontal = 6.dp).padding(top = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        for (ch in shown) {
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
