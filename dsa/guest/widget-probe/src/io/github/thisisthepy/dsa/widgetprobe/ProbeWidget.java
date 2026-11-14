package io.github.thisisthepy.dsa.widgetprobe;

import android.appwidget.AppWidgetManager;
import android.appwidget.AppWidgetProvider;
import android.content.Context;
import android.widget.RemoteViews;

/** A widget of the probe: a coloured box with its name. The manifest says what each setup does. */
public class ProbeWidget extends AppWidgetProvider {
    @Override
    public void onUpdate(Context context, AppWidgetManager manager, int[] ids) {
        RemoteViews views = new RemoteViews(context.getPackageName(), R.layout.widget);
        views.setTextViewText(R.id.text, getClass().getSimpleName());
        manager.updateAppWidget(ids, views);
    }
}
