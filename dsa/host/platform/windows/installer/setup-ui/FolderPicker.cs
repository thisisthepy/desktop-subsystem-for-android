namespace Dsa.Setup
{
    using System;
    using System.IO;
    using System.Runtime.InteropServices;

    /// <summary>The Windows folder picker (the common item dialog in folder mode).</summary>
    internal static class FolderPicker
    {
        private const uint FosPickFolders = 0x20;
        private const uint FosForceFileSystem = 0x40;
        private const uint FosPathMustExist = 0x800;
        private const uint SigdnFileSysPath = 0x80058000;
        private const int ErrorCancelled = unchecked((int)0x800704C7);

        /// <summary>Asks for a folder, starting at <paramref name="start"/>; null when cancelled.</summary>
        public static string Pick(IntPtr owner, string title, string start)
        {
            var dialog = (IFileOpenDialog)new FileOpenDialog();
            try
            {
                dialog.GetOptions(out var options);
                dialog.SetOptions(options | FosPickFolders | FosForceFileSystem | FosPathMustExist);
                dialog.SetTitle(title);

                var existing = NearestExisting(start);
                if (existing != null)
                {
                    var iid = typeof(IShellItem).GUID;
                    if (SHCreateItemFromParsingName(existing, IntPtr.Zero, ref iid, out var folder) == 0)
                    {
                        dialog.SetFolder(folder);
                        Marshal.ReleaseComObject(folder);
                    }
                }

                int hr = dialog.Show(owner);
                if (hr == ErrorCancelled)
                {
                    return null;
                }

                Marshal.ThrowExceptionForHR(hr);
                dialog.GetResult(out var item);
                try
                {
                    item.GetDisplayName(SigdnFileSysPath, out var path);
                    return path;
                }
                finally
                {
                    Marshal.ReleaseComObject(item);
                }
            }
            finally
            {
                Marshal.ReleaseComObject(dialog);
            }
        }

        /// <summary>The folder itself if it exists, else its nearest parent that does.</summary>
        private static string NearestExisting(string path)
        {
            try
            {
                var dir = string.IsNullOrWhiteSpace(path) ? null : new DirectoryInfo(path);
                while (dir != null && !dir.Exists)
                {
                    dir = dir.Parent;
                }

                return dir?.FullName;
            }
            catch (Exception)
            {
                return null;
            }
        }

        [ComImport, Guid("DC1C5A9C-E88A-4dde-A5A1-60F82A20AEF7")]
        private class FileOpenDialog
        {
        }

        [ComImport, Guid("d57c7288-d4ad-4768-be02-9d969532d960"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IFileOpenDialog
        {
            // IModalWindow
            [PreserveSig] int Show(IntPtr parent);

            // IFileDialog
            void SetFileTypes(uint count, IntPtr specs);
            void SetFileTypeIndex(uint index);
            void GetFileTypeIndex(out uint index);
            void Advise(IntPtr events, out uint cookie);
            void Unadvise(uint cookie);
            void SetOptions(uint options);
            void GetOptions(out uint options);
            void SetDefaultFolder(IShellItem item);
            void SetFolder(IShellItem item);
            void GetFolder(out IShellItem item);
            void GetCurrentSelection(out IShellItem item);
            void SetFileName([MarshalAs(UnmanagedType.LPWStr)] string name);
            void GetFileName([MarshalAs(UnmanagedType.LPWStr)] out string name);
            void SetTitle([MarshalAs(UnmanagedType.LPWStr)] string title);
            void SetOkButtonLabel([MarshalAs(UnmanagedType.LPWStr)] string text);
            void SetFileNameLabel([MarshalAs(UnmanagedType.LPWStr)] string label);
            void GetResult(out IShellItem item);
            void AddPlace(IShellItem item, int place);
            void SetDefaultExtension([MarshalAs(UnmanagedType.LPWStr)] string extension);
            void Close(int hr);
            void SetClientGuid(ref Guid guid);
            void ClearClientData();
            void SetFilter(IntPtr filter);

            // IFileOpenDialog
            void GetResults(out IntPtr items);
            void GetSelectedItems(out IntPtr items);
        }

        [ComImport, Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IShellItem
        {
            void BindToHandler(IntPtr bindContext, ref Guid handler, ref Guid iid, out IntPtr result);
            void GetParent(out IShellItem parent);
            void GetDisplayName(uint form, [MarshalAs(UnmanagedType.LPWStr)] out string name);
            void GetAttributes(uint mask, out uint attributes);
            void Compare(IShellItem other, uint hint, out int order);
        }

        [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
        private static extern int SHCreateItemFromParsingName(string path, IntPtr bindContext, ref Guid iid, out IShellItem item);
    }
}
