package lol.agu.chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The desktop's split header, on a phone: one line under the tabs with a LIVE badge while a channel of the tab is live
 * and the stream's title (the live channel's, else the first one that has a title). A long title is cut to the line;
 * tapping it shows the whole of it. Nothing shows for a tab that is offline and has no title, or a whisper tab.
 */
@Composable
fun ChannelHeader(vm: ChatViewModel, tab: Tab) {
    if (tab.whisperOnly) return
    val ch = tab.channels.mapNotNull { c -> vm.conns[c.sk()]?.let { c to it } }
    val live = ch.firstOrNull { it.second.live == true }
    val from = live?.takeIf { !it.second.title.isNullOrBlank() } ?: ch.firstOrNull { !it.second.title.isNullOrBlank() }
    val title = from?.second?.title?.trim().orEmpty()
    if (live == null && title.isEmpty()) return
    var open by remember(tab.id, title) { mutableStateOf(false) }
    // a merged tab says whose title it is
    val text = if (tab.channels.size > 1 && from != null) "${from.first.shown()}: $title" else title
    Row(
        Modifier.fillMaxWidth().clickable(enabled = title.isNotEmpty()) { open = !open }.padding(horizontal = 12.dp, vertical = 5.dp),
        verticalAlignment = if (open) Alignment.Top else Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (live != null) Text(
            "LIVE", color = Color.White, fontSize = 10.sp, fontWeight = FontWeight.Bold, letterSpacing = 0.6.sp,
            modifier = Modifier.padding(top = if (open) 1.dp else 0.dp).clip(RoundedCornerShape(3.dp)).background(Color(0xFFDC2626)).padding(horizontal = 5.dp, vertical = 1.dp),
        )
        if (title.isNotEmpty()) Text(text, color = AguColors.Muted, fontSize = 13.sp, maxLines = if (open) Int.MAX_VALUE else 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
    }
}
