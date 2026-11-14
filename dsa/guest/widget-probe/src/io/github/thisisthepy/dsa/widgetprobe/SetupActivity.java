package io.github.thisisthepy.dsa.widgetprobe;

import android.app.Activity;
import android.appwidget.AppWidgetManager;
import android.appwidget.AppWidgetProviderInfo;
import android.content.Intent;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.widget.TextView;

/**
 * The setup of the probe's widgets. It answers by itself, by the widget it sets up (see the
 * manifest): at once, or two seconds after its window first has the focus, which a setup
 * started where the user cannot see it never gets.
 */
public class SetupActivity extends Activity {
    private static final String TAG = "DsaWidgetProbe";
    private static final long ANSWER_AFTER_FOCUS_MS = 2000;

    private final Handler handler = new Handler(Looper.getMainLooper());
    private int widgetId = AppWidgetManager.INVALID_APPWIDGET_ID;
    private String kind = "";
    private boolean answering;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        widgetId = getIntent().getIntExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, AppWidgetManager.INVALID_APPWIDGET_ID);
        AppWidgetProviderInfo info = AppWidgetManager.getInstance(this).getAppWidgetInfo(widgetId);
        kind = info == null ? "" : info.provider.getClassName();
        Log.i(TAG, "setup of widget " + widgetId + " (" + kind + ") started in task " + getTaskId());
        // Canceled unless it says otherwise, as a launcher reads a setup that just goes.
        setResult(RESULT_CANCELED, answer());
        if (kind.endsWith(".AtOnceWidget")) {
            finishWith(RESULT_OK);
            return;
        }
        TextView text = new TextView(this);
        text.setText("Setting up " + kind);
        text.setPadding(48, 48, 48, 48);
        setContentView(text);
    }

    @Override
    public void onWindowFocusChanged(boolean focused) {
        super.onWindowFocusChanged(focused);
        if (!focused || answering) return;
        answering = true;
        Log.i(TAG, "setup of widget " + widgetId + " has the focus");
        final int result = kind.endsWith(".CancelWidget") ? RESULT_CANCELED : RESULT_OK;
        handler.postDelayed(() -> finishWith(result), ANSWER_AFTER_FOCUS_MS);
    }

    private void finishWith(int result) {
        Log.i(TAG, "setup of widget " + widgetId + " answers " + (result == RESULT_OK ? "OK" : "canceled"));
        setResult(result, answer());
        finish();
    }

    private Intent answer() {
        return new Intent().putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId);
    }
}
