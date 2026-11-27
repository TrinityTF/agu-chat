package lol.agu.chat.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The emote picker, a bottom sheet like the desktop's popup: a tab for each source the chats have (Twitch's own emotes,
 * 7TV, BTTV, FFZ, Kick, YouTube, Rumble) and a search over all of them. Tap puts the emote in the message and keeps
 * the sheet open for more; long-press names it. [preview] is the message so far.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalFoundationApi::class)
@Composable
fun EmotePickerSheet(vm: ChatViewModel, sks: List<String>, preview: String, onInsert: (String) -> Unit, onDismiss: () -> Unit) {
    val animated = LocalDisplay.current.animatedEmotes
    var tab by remember { mutableStateOf("") }
    var q by remember { mutableStateOf("") }
    var data by remember { mutableStateOf(PickerData()) }
    var loaded by remember { mutableStateOf(false) }
    var named by remember { mutableStateOf<PickerItem?>(null) }
    // the first answer only brings the tabs ("-"), then the first tab opens
    LaunchedEffect(tab, q, sks) {
        val r = vm.picker(sks, if (tab.isEmpty()) "-" else tab, q.trim())
        data = r
        loaded = true
        if (tab.isEmpty() && r.tabs.isNotEmpty()) tab = r.tabs.first().id
    }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = AguColors.Surface, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().imePadding().navigationBarsPadding().padding(bottom = 8.dp)) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.weight(1f).clip(RoundedCornerShape(22.dp)).background(AguColors.Surface2).padding(horizontal = 16.dp, vertical = 10.dp)) {
                    if (q.isEmpty()) Text("Search emotes", color = AguColors.Muted, fontSize = 15.sp)
                    BasicTextField(
                        q, { q = it }, singleLine = true,
                        textStyle = TextStyle(color = AguColors.Text, fontSize = 15.sp),
                        cursorBrush = SolidColor(AguColors.Accent),
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                TextButton(onClick = onDismiss) { Text("Done", color = AguColors.Accent, fontWeight = FontWeight.SemiBold) }
            }
            Text(
                named?.let { "${it.name}  ·  ${it.tip}" } ?: preview.ifEmpty { "Tap an emote to add it" },
                color = HintColor, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 18.dp, vertical = 6.dp),
            )
            if (q.isBlank() && data.tabs.size > 1) LazyRow(
                Modifier.fillMaxWidth().padding(bottom = 6.dp), contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(data.tabs) { t ->
                    val sel = t.id == tab
                    Text(
                        t.label, color = if (sel) androidx.compose.ui.graphics.Color.White else AguColors.Muted, fontSize = 14.sp, fontWeight = FontWeight.Medium,
                        modifier = Modifier.clip(RoundedCornerShape(18.dp)).background(if (sel) AguColors.Accent else AguColors.Surface2)
                            .clickable { tab = t.id }.padding(horizontal = 14.dp, vertical = 8.dp),
                    )
                }
            }
            if (loaded && data.sections.isEmpty()) {
                Box(Modifier.fillMaxWidth().height(200.dp), contentAlignment = Alignment.Center) {
                    Text(if (data.tabs.isEmpty() && q.isBlank()) "No emotes loaded yet" else "No emotes found.", color = AguColors.Muted, fontSize = 14.sp)
                }
            } else {
                LazyVerticalGrid(
                    GridCells.Adaptive(52.dp), Modifier.fillMaxWidth().heightIn(max = 420.dp),
                    contentPadding = PaddingValues(horizontal = 12.dp),
                ) {
                    for (s in data.sections) {
                        item(key = "h:${s.id}", span = { GridItemSpan(maxLineSpan) }) {
                            Text(s.label.uppercase(), color = HintColor, fontSize = 11.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(start = 6.dp, top = 10.dp, bottom = 4.dp))
                        }
                        items(s.items, key = { "${s.id}:${it.key}" }) { e ->
                            Box(
                                Modifier.size(52.dp).clip(RoundedCornerShape(8.dp)).combinedClickable(onClick = { named = null; onInsert(e.insert) }, onLongClick = { named = e }),
                                contentAlignment = Alignment.Center,
                            ) { EmoteImage(e.img, animated, Modifier.size(36.dp)) }
                        }
                    }
                }
            }
        }
    }
}

/** What you sent before in this tab, newest first; tap one to put it back in the box. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HistorySheet(items: List<String>, onPick: (String) -> Unit, onDismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = AguColors.Surface) {
        Column(Modifier.navigationBarsPadding().padding(bottom = 12.dp)) {
            Text("Recent messages", color = AguColors.Muted, fontSize = 13.sp, modifier = Modifier.padding(horizontal = 20.dp, vertical = 8.dp))
            if (items.isEmpty()) Text("Nothing sent yet", color = HintColor, fontSize = 15.sp, modifier = Modifier.padding(horizontal = 20.dp, vertical = 14.dp))
            else androidx.compose.foundation.lazy.LazyColumn(Modifier.heightIn(max = 420.dp)) {
                items(items) { m ->
                    Text(m, color = AguColors.Text, fontSize = 15.sp, maxLines = 2, overflow = TextOverflow.Ellipsis, modifier = Modifier.fillMaxWidth().clickable { onPick(m) }.padding(horizontal = 20.dp, vertical = 12.dp))
                }
            }
        }
    }
}
