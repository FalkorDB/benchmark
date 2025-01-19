import Image from "next/image";
import Link from "next/link";
import { FaGithub } from "react-icons/fa";
import { IoHomeOutline } from "react-icons/io5";
import { AiOutlineDiscord } from "react-icons/ai";

export function Header() {
  return (
    <header className="flex flex-col text-xl bg-[#F7F3EF] h-20 z-20">
      <div className="flex items-center justify-between py-4 px-8">
        <div className="flex gap-4 items-center">
          <Link href="https://www.falkordb.com" target="_blank">
            <Image src="/logo.svg" alt="FalkorDB" width={130} height={25} />
          </Link>
          <div className="h-10 w-[2px] border-l border-black"></div>

          <h1 className="font-space font-bold text-[20px]">
            Graph Database Performance Benchmarks
          </h1>
        </div>
        <ul className="flex gap-4 items-center font-medium bg-white rounded-lg shadow p-4 h-14">
          <Link
            title="Home"
            className="flex gap-2 items-center pl-1"
            href="https://www.falkordb.com"
            target="_blank"
          >
            <IoHomeOutline size={25} />
          </Link>
          <Link
            title="Github"
            className="flex gap-2 items-center pl-1"
            href="https://github.com/FalkorDB/benchmark"
            target="_blank"
          >
            <FaGithub size={25} />
          </Link>
          <Link
            title="Discord"
            className="flex gap-2 items-center pl-2"
            href="https://discord.com/invite/99y2Ubh6tg"
            target="_blank"
          >
            <AiOutlineDiscord size={30} />
          </Link>
          <div className="h-7 w-[2px] border-l bg-gradient-to-r from-[#EC806C] via-[#B66EBD] to-[#7568F2]"></div>
          <a
            href="https://app.falkordb.cloud/signup"
            className="h-8 text-black font-medium text-base hover:underline flex text-center justify-center"
          >
            Sign up
          </a>
          <a
            href="https://falkordb.com/try-free/"
            className="h-10 bg-gradient-to-r from-[#EC806C] via-[#B66EBD] to-[#7568F2] text-white font-medium text-base px-4 py-2 rounded-lg shadow-md hover:opacity-90 transition-all duration-200 flex text-center justify-center"
          >
            Start Free
          </a>
        </ul>
      </div>
    </header>
  );
}
