import Image from "next/image";
import Link from "next/link";
import { FaGithub } from "react-icons/fa";
import { IoHomeOutline } from "react-icons/io5";
import { AiOutlineDiscord } from "react-icons/ai";

export function Header() {
  return (
    <header className="flex flex-col text-xl bg-[#F7F3EF] min-h-20 md:h-20 z-20">
      <div className="flex items-center justify-between gap-2 py-3 px-4 md:py-4 md:px-8">
        <div className="flex gap-2 md:gap-4 items-center min-w-0">
          <Link href="https://www.falkordb.com" target="_blank" className="shrink-0">
            <Image src="/logo.svg" alt="FalkorDB" width={130} height={25} className="w-24 md:w-[130px] h-auto" />
          </Link>
          <div className="hidden md:block h-10 w-[2px] border-l border-black"></div>

          <h1 className="hidden md:block font-space font-bold text-base lg:text-[20px] truncate">
            Graph Database Performance Benchmarks
          </h1>
        </div>
        <ul className="flex gap-2 md:gap-4 items-center font-medium bg-white rounded-lg shadow p-2 md:p-4 h-12 md:h-14 shrink-0">
          <Link
            title="Home"
            className="hidden sm:flex gap-2 items-center pl-1"
            href="https://www.falkordb.com"
            target="_blank"
          >
            <IoHomeOutline className="size-5 md:size-[25px]" />
          </Link>
          <Link
            title="Github"
            className="flex gap-2 items-center pl-1"
            href="https://github.com/FalkorDB/benchmark"
            target="_blank"
          >
            <FaGithub className="size-5 md:size-[25px]" />
          </Link>
          <Link
            title="Discord"
            className="flex gap-2 items-center pl-1 md:pl-2"
            href="https://discord.com/invite/99y2Ubh6tg"
            target="_blank"
          >
            <AiOutlineDiscord className="size-6 md:size-[30px]" />
          </Link>
          <div className="h-7 w-[2px] border-l bg-gradient-to-r from-[#EC806C] via-[#B66EBD] to-[#7568F2]"></div>
          <a
            href="https://app.falkordb.cloud/signup"
            className="hidden sm:flex h-8 text-black font-medium text-sm md:text-base hover:underline text-center justify-center items-center"
            target="_blank"
          >
            Sign up
          </a>
          <a
            href="https://falkordb.com/try-free/"
            className="h-9 md:h-10 bg-gradient-to-r from-[#EC806C] via-[#B66EBD] to-[#7568F2] text-white font-medium text-sm md:text-base px-3 md:px-4 py-2 rounded-lg shadow-md hover:opacity-90 transition-all duration-200 flex text-center justify-center items-center whitespace-nowrap"
            target="_blank"
          >
            Start Free
          </a>
        </ul>
      </div>
    </header>
  );
}
