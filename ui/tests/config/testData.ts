import urls from '../config/urls.json';

export const headerItems: { navItem: string; expectedRes: string }[] = [
    { navItem: "Home", expectedRes: urls.falkorDBUrl },
    { navItem: "Github", expectedRes: urls.benchmarkGithubUrl },
    { navItem: "Discord", expectedRes: urls.falkordbDiscordUrl },
    { navItem: "Sign up", expectedRes: urls.signUpUrl },
    { navItem: "Start Free", expectedRes: urls.startFreeUrl },
  ];


  export const footerItems: { item: string; expectedRes: string }[] = [
    { item: "DATASET USED", expectedRes: urls.DatasetUrl },
    { item: "README", expectedRes: urls.ReadmeUrl },
    { item: "FAQ", expectedRes: urls.FAQUrl },
    { item: "RUN THE BENCHMARK", expectedRes: urls.runBenchmarkWithYourDataUrl },
  ];