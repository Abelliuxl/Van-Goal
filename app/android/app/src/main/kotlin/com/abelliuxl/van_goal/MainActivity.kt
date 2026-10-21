package com.abelliuxl.van_goal

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import android.app.Activity
import android.content.ContentValues
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import java.io.File
import java.util.UUID
import java.util.concurrent.Executors

class MainActivity : FlutterActivity() {
    private val filesWorker = Executors.newSingleThreadExecutor()
    private var picking: MethodChannel.Result? = null
    private var maximum = 6
    private var saving: Pair<File, MethodChannel.Result>? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "com.abelliuxl.van_goal/files")
            .setMethodCallHandler { call, result ->
                try {
                    when (call.method) {
                        "pick" -> {
                            check(picking == null) { "文件选择器已打开" }
                            maximum = (call.argument<Int>("maximum") ?: 6).coerceIn(1, 6)
                            picking = result
                            try {
                                startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                                    addCategory(Intent.CATEGORY_OPENABLE)
                                    type = if (call.argument<Boolean>("images") == true) "image/*" else "*/*"
                                    putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                                    addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                                }, 701)
                            } catch (error: Exception) { picking = null; throw error }
                        }
                        "importImage" -> {
                            val uri = call.argument<String>("uri") ?: ""
                            val bytes = call.argument<ByteArray>("data")
                            val mime = call.argument<String>("mime") ?: "image/png"
                            worker(result) {
                                if (bytes != null && bytes.isNotEmpty()) {
                                    require(bytes.size <= 8 * 1024 * 1024) { "图片超过 8 MB" }
                                    val ext = MimeTypeMap.getSingleton().getExtensionFromMimeType(mime) ?: "png"
                                    val destination = attachmentDestination("Clipboard.$ext")
                                    destination.writeBytes(bytes)
                                    filePayload(destination)
                                } else importUri(Uri.parse(uri))
                            }
                        }
                        "save" -> {
                            val source = privateFile(call.argument<String>("path") ?: "")
                            val name = safeName(call.argument<String>("name") ?: "Attachment")
                            if (Build.VERSION.SDK_INT >= 29) {
                                worker(result) {
                                    val values = ContentValues().apply {
                                        put(MediaStore.Downloads.DISPLAY_NAME, name)
                                        put(MediaStore.Downloads.MIME_TYPE, mime(name))
                                        put(MediaStore.Downloads.RELATIVE_PATH, "${Environment.DIRECTORY_DOWNLOADS}/Van-Goal")
                                        put(MediaStore.Downloads.IS_PENDING, 1)
                                    }
                                    val uri = contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                                        ?: error("无法创建下载文件")
                                    try {
                                        contentResolver.openOutputStream(uri)?.use { out -> source.inputStream().use { it.copyTo(out) } }
                                            ?: error("无法写入下载文件")
                                        values.clear(); values.put(MediaStore.Downloads.IS_PENDING, 0)
                                        contentResolver.update(uri, values, null, null)
                                        uri.toString()
                                    } catch (error: Exception) { contentResolver.delete(uri, null, null); throw error }
                                }
                            } else {
                                check(saving == null) { "保存窗口已打开" }
                                saving = Pair(source, result)
                                try {
                                    startActivityForResult(Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                                        addCategory(Intent.CATEGORY_OPENABLE); type = mime(name)
                                        putExtra(Intent.EXTRA_TITLE, name)
                                        if (Build.VERSION.SDK_INT >= 26) putExtra(DocumentsContract.EXTRA_INITIAL_URI,
                                            Uri.parse("content://com.android.externalstorage.documents/document/primary%3ADownload"))
                                    }, 702)
                                } catch (error: Exception) { saving = null; throw error }
                            }
                        }
                        "open" -> {
                            val uri = Uri.parse(call.argument<String>("uri"))
                            require(uri.scheme == "content") { "无效的文件地址" }
                            startActivity(Intent(Intent.ACTION_VIEW).apply {
                                setDataAndType(uri, mime(call.argument<String>("name") ?: ""))
                                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                            })
                            result.success(null)
                        }
                        else -> result.notImplemented()
                    }
                } catch (error: Exception) { result.error("files", error.localizedMessage, null) }
            }
    }

    private fun worker(result: MethodChannel.Result, action: () -> Any?) {
        filesWorker.execute {
            try { val value = action(); runOnUiThread { result.success(value) } }
            catch (error: Exception) { runOnUiThread { result.error("files", error.localizedMessage, null) } }
        }
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == 701) {
            val result = picking ?: return
            picking = null
            if (resultCode != Activity.RESULT_OK || data == null) { result.success(emptyList<Any>()); return }
            val uris = mutableListOf<Uri>()
            data.clipData?.let { clip -> for (i in 0 until clip.itemCount) uris.add(clip.getItemAt(i).uri) }
            if (uris.isEmpty()) data.data?.let { uris.add(it) }
            val limit = maximum
            worker(result) {
                require(uris.size <= limit) { "本次最多选择 $limit 个附件" }
                val imported = mutableListOf<Map<String, Any>>()
                try { uris.distinct().forEach { imported.add(importUri(it)) }; imported }
                catch (error: Exception) { imported.forEach { File(it["path"] as String).delete() }; throw error }
            }
        } else if (requestCode == 702) {
            val pending = saving ?: return
            saving = null
            val uri = data?.data
            if (resultCode != Activity.RESULT_OK || uri == null) { pending.second.success(null); return }
            worker(pending.second) {
                contentResolver.openOutputStream(uri)?.use { out -> pending.first.inputStream().use { it.copyTo(out) } }
                    ?: error("无法写入文件")
                uri.toString()
            }
        }
    }

    private fun safeName(name: String): String = name.substringAfterLast('/').substringAfterLast('\\')
        .filter { !it.isISOControl() }.take(180).ifBlank { "Attachment" }

    private fun attachmentDestination(name: String): File {
        val dir = File(filesDir, "Attachments/${UUID.randomUUID()}")
        check(dir.mkdirs()) { "无法创建附件目录" }
        return File(dir, safeName(name))
    }

    private fun importUri(uri: Uri): Map<String, Any> {
        require(uri.scheme == "content") { "请选择可读取的文件" }
        var name = "Attachment"
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                name = cursor.getString(0) ?: name
                if (!cursor.isNull(1)) require(cursor.getLong(1) <= 24L * 1024 * 1024) { "附件超过 24 MB" }
            }
        }
        val type = contentResolver.getType(uri) ?: ""
        if (!name.contains('.') && type.startsWith("image/")) {
            name += ".${MimeTypeMap.getSingleton().getExtensionFromMimeType(type) ?: "png"}"
        }
        val destination = attachmentDestination(name)
        try {
            contentResolver.openInputStream(uri)?.use { input ->
                destination.outputStream().use { output ->
                    val buffer = ByteArray(64 * 1024); var total = 0L
                    val limit = if (type in listOf("image/png", "image/jpeg", "image/gif", "image/webp")) 8L * 1024 * 1024 else 24L * 1024 * 1024
                    while (true) { val count = input.read(buffer); if (count < 0) break
                        total += count; require(total <= limit) { "附件超过 ${limit / 1024 / 1024} MB" }; output.write(buffer, 0, count)
                    }
                }
            } ?: error("无法读取文件")
        } catch (error: Exception) { destination.delete(); throw error }
        return filePayload(destination)
    }

    private fun filePayload(file: File): Map<String, Any> = mapOf("path" to file.path, "name" to file.name, "remote" to false)
    private fun privateFile(path: String): File {
        val file = File(path).canonicalFile
        require(file.path.startsWith(filesDir.canonicalPath + File.separator) && file.isFile) { "无法读取应用内附件" }
        return file
    }
    private fun mime(name: String): String = MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase()) ?: "application/octet-stream"
}
