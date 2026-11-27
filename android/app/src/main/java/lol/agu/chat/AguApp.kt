package lol.agu.chat

import android.app.Application
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import coil3.gif.AnimatedImageDecoder
import coil3.memory.MemoryCache
import coil3.request.crossfade

class AguApp : Application(), SingletonImageLoader.Factory {
    override fun onCreate() {
        super.onCreate()
        Core.start(this)
    }

    /** Emotes and badges: animated WebP / GIF decoded by the platform, a memory cache sized for a busy chat. */
    override fun newImageLoader(context: PlatformContext): ImageLoader =
        ImageLoader.Builder(context)
            .components { add(AnimatedImageDecoder.Factory()) }
            .memoryCache { MemoryCache.Builder().maxSizePercent(context, 0.25).build() }
            .crossfade(false)
            .build()
}
