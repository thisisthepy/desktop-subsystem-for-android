namespace Dsa.Setup
{
    using WixToolset.BootstrapperApplicationApi;

    internal static class Program
    {
        /// <summary>
        /// Burn starts this process and talks to it over a pipe; Run returns when the engine
        /// is done with the application.
        /// </summary>
        private static int Main()
        {
            ManagedBootstrapperApplication.Run(new SetupApplication());
            return 0;
        }
    }
}
