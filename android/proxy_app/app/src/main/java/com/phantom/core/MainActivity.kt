package com.phantom.core

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import org.json.JSONObject

class MainActivity : AppCompatActivity() {

    private var proxyService: ProxyService? = null
    private var isBound = false
    private var isTethered = false

    private lateinit var btnTether: Button
    private lateinit var etIpAddress: EditText
    private lateinit var tvStatus: TextView
    private lateinit var tvLatency: TextView
    private lateinit var tvThroughput: TextView
    private lateinit var tvPackets: TextView

    private val statsHandler = Handler(Looper.getMainLooper())
    private val statsRunnable = object : Runnable {
        override fun run() {
            updateStats()
            statsHandler.postDelayed(this, 500)
        }
    }

    private val serviceConnection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, service: IBinder?) {
            val binder = service as ProxyService.ProxyBinder
            proxyService = binder.getService()
            isBound = true
        }

        override fun onServiceDisconnected(name: ComponentName?) {
            proxyService = null
            isBound = false
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        btnTether = findViewById(R.id.btn_tether)
        etIpAddress = findViewById(R.id.et_ip_address)
        tvStatus = findViewById(R.id.tv_status)
        tvLatency = findViewById(R.id.tv_latency)
        tvThroughput = findViewById(R.id.tv_throughput)
        tvPackets = findViewById(R.id.tv_packets)

        etIpAddress.setText("192.168.1.100")

        btnTether.setOnClickListener {
            if (isTethered) {
                stopTether()
            } else {
                startTether()
            }
        }
    }

    private fun startTether() {
        val ip = etIpAddress.text.toString().trim()
        if (ip.isEmpty()) {
            tvStatus.text = "⚠ Enter PC IP address"
            return
        }

        val intent = Intent(this, ProxyService::class.java).apply {
            putExtra(ProxyService.EXTRA_DAEMON_IP, ip)
            putExtra(ProxyService.EXTRA_DAEMON_PORT, ProxyService.DEFAULT_PORT)
        }
        startForegroundService(intent)
        bindService(intent, serviceConnection, Context.BIND_AUTO_CREATE)

        isTethered = true
        btnTether.text = "⬛ UNTETHER"
        btnTether.setBackgroundColor(0xFFFF4444.toInt())
        tvStatus.text = "🟢 TETHERED to $ip"
        etIpAddress.isEnabled = false

        statsHandler.postDelayed(statsRunnable, 500)
    }

    private fun stopTether() {
        statsHandler.removeCallbacks(statsRunnable)

        if (isBound) {
            unbindService(serviceConnection)
            isBound = false
        }
        stopService(Intent(this, ProxyService::class.java))

        isTethered = false
        btnTether.text = "⚡ TETHER"
        btnTether.setBackgroundColor(0xFF00CC88.toInt())
        tvStatus.text = "⚫ DISCONNECTED"
        tvLatency.text = "Latency: —"
        tvThroughput.text = "Throughput: —"
        tvPackets.text = "Packets: —"
        etIpAddress.isEnabled = true
    }

    private fun updateStats() {
        val statsJson = proxyService?.getStats() ?: return
        try {
            val stats = JSONObject(statsJson)
            val sent = stats.optLong("sent", 0)
            val received = stats.optLong("received", 0)
            val avgLatency = stats.optDouble("avg_latency_us", 0.0)

            tvLatency.text = "Latency: %.1f µs".format(avgLatency)
            tvThroughput.text = "Throughput: $sent sent / $received recv"
            tvPackets.text = "Packets: ${sent + received} total"
        } catch (e: Exception) {
            tvLatency.text = "Latency: error"
        }
    }

    override fun onDestroy() {
        super.onDestroy()
        statsHandler.removeCallbacks(statsRunnable)
        if (isBound) {
            unbindService(serviceConnection)
        }
    }
}
