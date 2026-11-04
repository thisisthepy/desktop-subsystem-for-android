using Xunit;

namespace Dsa.Settings.Core.Tests;

public class StoragePathsTests
{
    [Fact]
    public void Fr19_user_logs_are_in_the_users_dsa_folder()
    {
        var local = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        Assert.Equal(Path.Combine(local, "DSA", "logs"), StoragePaths.UserLogs);
    }

    [Fact]
    public void Fr19_service_logs_follow_the_service_then_the_installers_data_folder()
    {
        Assert.Equal(@"D:\Logs", StoragePaths.MachineLogs(@"D:\Logs", @"E:\Data"));
        Assert.Equal(Path.Combine(@"E:\Data", "logs"), StoragePaths.MachineLogs("", @"E:\Data"));
        Assert.Equal(Path.Combine(StoragePaths.DefaultMachineRoot, "logs"), StoragePaths.MachineLogs(null));
    }
}
