package org.rustgis.mobile;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import java.io.File;
import java.io.FileNotFoundException;

/**
 * Where the camera app writes a photo for RustGIS: the app's own photos folder, offered by a
 * content address (content://org.rustgis.mobile.photos/NAME.jpg), as Android requires of a file
 * handed to another app. The activity grants the camera app leave to write the one address it
 * asks for (MainActivity.takePhoto); the provider itself is not exported.
 */
public class PhotoProvider extends ContentProvider {
    static final String AUTHORITY = "org.rustgis.mobile.photos";

    /** The app's photos folder. */
    static File folder(android.content.Context context) {
        return new File(context.getFilesDir(), "photos");
    }

    /** The file an address names: a plain name in the photos folder, or none. */
    private File file(Uri uri) throws FileNotFoundException {
        String name = uri.getLastPathSegment();
        if (getContext() == null || name == null || !name.matches("[A-Za-z0-9_.-]+") || name.startsWith(".")) {
            throw new FileNotFoundException("no such photo: " + uri);
        }
        return new File(folder(getContext()), name);
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        File file = file(uri);
        File dir = file.getParentFile();
        if (dir != null && !dir.isDirectory() && !dir.mkdirs()) {
            throw new FileNotFoundException("the app's storage can't be written to");
        }
        return ParcelFileDescriptor.open(file, ParcelFileDescriptor.parseMode(mode));
    }

    @Override
    public String getType(Uri uri) {
        return "image/jpeg";
    }

    /** The name and size some camera apps ask for before writing. */
    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args, String sort) {
        File file;
        try {
            file = file(uri);
        } catch (FileNotFoundException e) {
            return null;
        }
        MatrixCursor cursor = new MatrixCursor(new String[] {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE});
        cursor.addRow(new Object[] {file.getName(), file.length()});
        return cursor;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        return null;
    }

    @Override
    public int delete(Uri uri, String selection, String[] args) {
        return 0;
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] args) {
        return 0;
    }
}
