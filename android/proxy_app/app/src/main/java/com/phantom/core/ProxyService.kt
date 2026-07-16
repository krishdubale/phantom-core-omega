package com.phantom.core

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.os.Binder
import android.os.Build
import android.os.IBinder
import android.util.Log

class ProxyService : Service() {

    companion object {
        const val TAG = "PhantomProxy"
        const val CHANNEL_ID = "phantom_core_channel"
        const val NOTIFICATION_ID = 1337
        const val EXTRA_DAEMON_IP = "daemon_ip"
        const val EXTRA_DAEMON_PORT = "daemon_port"
        const val DEFAULT_PORT = 42069
    }

    private val binder = ProxyBinder()
    private var isRunning = false

    inner class ProxyBinder : Binder() {
        fun getService(): ProxyService = this@ProxyService
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onCreate() {
        super.onCreate()
        System.loadLibrary("phantom-proxy")
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val ip = intent?.getStringExtra(EXTRA_DAEMON_IP) ?: "192.168.1.100"
        val port = intent?.getIntExtra(EXTRA_DAEMON_PORT, DEFAULT_PORT) ?: DEFAULT_PORT

        val notification = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("PhantomCore Active")
            .setContentText("Offloading to $ip:$port")
            .setSmallIcon(android.R.drawable.ic_menu_send)
            .setOngoing(true)
            .build()

        startForeground(NOTIFICATION_ID, notification)

        val success = nativeStartProxy(ip, port)
        isRunning = success
        Log.i(TAG, "Proxy start result: $success -> $ip:$port")

        return START_STICKY
    }

    override fun onDestroy() {
        super.onDestroy()
        if (isRunning) {
            nativeStopProxy()
            isRunning = false
        }
        Log.i(TAG, "ProxyService destroyed")
    }

    fun getStats(): String {
        return if (isRunning) nativeGetStats() else "{\"running\":0}"
    }

    fun isProxyRunning(): Boolean = isRunning

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "PhantomCore Offloading",
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = "Shows when PhantomCore is actively offloading compute"
            }
            val manager = getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(channel)
        }
    }

    // Native methods
    private external fun nativeStartProxy(daemonIp: String, port: Int): Boolean
    private external fun nativeStopProxy()
    private external fun nativeGetStats(): String
}
