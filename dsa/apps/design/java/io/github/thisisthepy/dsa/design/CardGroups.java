/*
 * SPDX-FileCopyrightText: The Desktop Subsystem for Android authors
 * SPDX-License-Identifier: Apache-2.0
 */

package io.github.thisisthepy.dsa.design;

import android.content.Context;
import android.content.res.TypedArray;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.Rect;
import android.graphics.RectF;
import android.view.View;

import androidx.recyclerview.widget.RecyclerView;

/**
 * Draws the rows of a list as rounded cards: each run of rows between two headers is one card,
 * with space between cards and none between the rows of a card. A list without headers is one
 * card. The card is drawn under the rows, so a row's own pressed state and dividers stay
 * inside it.
 */
public final class CardGroups extends RecyclerView.ItemDecoration {
    /** Says which adapter positions are headers (a preference category's title, say). */
    public interface Headers {
        boolean isHeader(int position);
    }

    /** A list with no headers: every row is in the one card. */
    public static final Headers NONE = position -> false;

    private final Headers headers;
    private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Path path = new Path();
    private final RectF rect = new RectF();
    private final float corner;
    private final int side;
    private final int gap;

    public CardGroups(Context context, Headers headers) {
        this.headers = headers;
        final float density = context.getResources().getDisplayMetrics().density;
        corner = 20 * density;
        side = Math.round(16 * density);
        gap = Math.round(12 * density);
        paint.setColor(themeColor(context, "colorSurfaceContainer",
                themeColor(context, "colorSurfaceVariant", Color.LTGRAY)));
    }

    /** Adds the cards to `list`. */
    public static void attach(RecyclerView list, Headers headers) {
        list.addItemDecoration(new CardGroups(list.getContext(), headers));
    }

    /**
     * A colour of the context's theme, by the attribute's name. The name is looked up rather
     * than compiled in, so this class builds in every app whatever version of the Material
     * library it has: a library without the attribute gives `fallback`.
     */
    private static int themeColor(Context context, String name, int fallback) {
        final int attr = context.getResources().getIdentifier(name, "attr", context.getPackageName());
        if (attr == 0) {
            return fallback;
        }
        final TypedArray values = context.obtainStyledAttributes(new int[] {attr});
        try {
            return values.getColor(0, fallback);
        } finally {
            values.recycle();
        }
    }

    private boolean inCard(RecyclerView.Adapter<?> adapter, int position) {
        return position >= 0 && position < adapter.getItemCount() && !headers.isHeader(position);
    }

    @Override
    public void getItemOffsets(Rect outRect, View view, RecyclerView parent, RecyclerView.State state) {
        final RecyclerView.Adapter<?> adapter = parent.getAdapter();
        final int position = parent.getChildAdapterPosition(view);
        if (adapter == null || !inCard(adapter, position)) {
            return;
        }
        outRect.left = side;
        outRect.right = side;
        if (!inCard(adapter, position + 1)) {
            outRect.bottom = gap;
        }
    }

    @Override
    public void onDraw(Canvas canvas, RecyclerView parent, RecyclerView.State state) {
        final RecyclerView.Adapter<?> adapter = parent.getAdapter();
        if (adapter == null) {
            return;
        }
        for (int i = 0; i < parent.getChildCount(); i++) {
            final View row = parent.getChildAt(i);
            final int position = parent.getChildAdapterPosition(row);
            if (!inCard(adapter, position)) {
                continue;
            }
            final float top = inCard(adapter, position - 1) ? 0 : corner;
            final float bottom = inCard(adapter, position + 1) ? 0 : corner;
            rect.set(row.getLeft() + row.getTranslationX(), row.getTop() + row.getTranslationY(),
                    row.getRight() + row.getTranslationX(), row.getBottom() + row.getTranslationY());
            path.reset();
            path.addRoundRect(rect, new float[] {top, top, top, top, bottom, bottom, bottom, bottom},
                    Path.Direction.CW);
            canvas.drawPath(path, paint);
        }
    }
}
