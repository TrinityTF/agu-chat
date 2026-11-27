package lol.agu.chat

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Text
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/** Smoke test of the Rust bridge: opens one Twitch chat and lists its lines as plain text. */
class MainActivity : ComponentActivity() {
    private val lines = mutableStateListOf<String>()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        lifecycleScope.launch {
            Core.events.collect { e ->
                if (e.kind != "rows") {
                    lines.add("[${e.kind}] ${e.data.toString().take(160)}")
                    return@collect
                }
                for (r in e.data.jsonObject["upsert"]!!.jsonArray) {
                    val ro = r.jsonObject
                    val name = ro["user"]?.jsonObject?.get("name")?.jsonPrimitive?.content ?: ""
                    val text = ro["parts"]?.jsonArray?.joinToString("") { p ->
                        val po = p.jsonObject
                        if (po["t"]?.jsonPrimitive?.content == "emote") "[${po["name"]!!.jsonPrimitive.content}]" else po["s"]?.jsonPrimitive?.content ?: ""
                    } ?: (ro["system"]?.jsonPrimitive?.content ?: "")
                    lines.add("$name: $text")
                }
            }
        }
        lifecycleScope.launch {
            runCatching { Core.call("chat.open", JsonObject(mapOf("platform" to JsonPrimitive("twitch"), "channel" to JsonPrimitive("a_g_u__")))) }
                .onFailure { lines.add("open failed: ${it.message}") }
        }
        setContent {
            LazyColumn(Modifier.fillMaxSize().background(Color(0xFF0E0E10))) {
                items(lines) { Text(it, color = Color.White) }
            }
        }
    }
}
