import urls from '../config/urls.json';

export const navitems: { navItem: string; expectedRes: string }[] = [
    { navItem: "Home", expectedRes: urls.falkorDBUrl },
    { navItem: "Github", expectedRes: urls.benchmarkGithubUrl },
    { navItem: "Discord", expectedRes: urls.falkordbDiscordUrl },
    { navItem: "Sign up", expectedRes: urls.signUpUrl },
    { navItem: "Start Free", expectedRes: urls.startFreeUrl },
  ];