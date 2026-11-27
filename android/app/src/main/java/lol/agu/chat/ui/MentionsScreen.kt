package lol.agu.chat.ui

import android.widget.Toast
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

private val timeFmt = DateTimeFormatter.ofPattern("HH:mm").withZone(ZoneId.systemDefault())

/** Every message that mentioned you or matched a highlight in any open chat, newest first; tap one to jump to it. */
@Composable
fun MentionsScreen(vm: ChatViewModel) {
    BackHandler { vm.closeMentions() }
    val ctx = LocalContext.current
    val entries = vm.mentionEntries
    Column(Modifier.fillMaxSize().background(AguColors.Bg).statusBarsPadding().navigationBarsPadding()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                "←", color = AguColors.Text, fontSize = 24.sp,
                modifier = Modifier.clip(CircleShape).clickable { vm.closeMentions() }.padding(horizontal = 14.dp, vertical = 6.dp),
            )
            Text("Mentions", color = AguColors.Text, fontSize = 20.sp, fontWeight = FontWeight.Bold, modifier = Modifier.weight(1f))
            if (entries.isNotEmpty()) Text(
                if (entries.size == 1) "1 message" else "${entries.size} messages",
                color = AguColors.Muted, fontSize = 12.sp, modifier = Modifier.padding(end = 16.dp),
            )
        }
        if (entries.isEmpty()) {
            Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                Text("No mentions yet", color = AguColors.Muted, fontSize = 14.sp)
            }
        } else {
            LazyColumn(Modifier.weight(1f)) {
                items(entries, key = { it.id }) { e ->
                    val go = { if (!vm.jumpToMention(e)) Toast.makeText(ctx, "That chat is no longer open", Toast.LENGTH_SHORT).show() }
                    Column(Modifier.fillMaxWidth().clickable(onClick = go).padding(vertical = 4.dp)) {
                        Row(Modifier.padding(horizontal = 8.dp, vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                            Box(Modifier.size(15.dp)) { PlatformMark(e.platform) }
                            Text(
                                if (e.whisper) "Whisper: ${e.channel}" else e.channel,
                                color = AguColors.Muted, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                                modifier = Modifier.padding(start = 6.dp).weight(1f), maxLines = 1,
                            )
                            Text(timeFmt.format(Instant.ofEpochMilli(e.ts)), color = AguColors.Muted, fontSize = 11.sp)
                        }
                        MessageRow(e.row, onLongPress = {}, onTap = go)
                    }
                    Box(Modifier.fillMaxWidth().size(1.dp).background(AguColors.Surface2))
                }
            }
        }
    }
}
