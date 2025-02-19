import { Sheet, SheetContent, SheetHeader, SheetTitle, SheetDescription } from "@/components/ui/sheet";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Checkbox } from "@/components/ui/checkbox";
import { useState } from "react";
import { CheckCircleIcon } from "@heroicons/react/24/solid";
import Image from "next/image";
import Logo from "../../public/logo.svg";

interface ContactUsSheetProps {
  isOpen: boolean;
  setIsOpen: (open: boolean) => void;
}

const isValidEmail = (email: string) => {
  const emailRegex = /^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/;
  return emailRegex.test(email);
};

export default function ContactUsSheet({ isOpen, setIsOpen }: ContactUsSheetProps) {
  const [step, setStep] = useState(1);
  const [selectedOptions, setSelectedOptions] = useState<string[]>([]);
  const [formData, setFormData] = useState({ name: "", email: "", company: "", message: "" });
  const [submitted, setSubmitted] = useState(false);

  const resetForm = () => {
    setStep(1);
    setSelectedOptions([]);
    setFormData({ name: "", email: "", company: "", message: "" });
    setSubmitted(false);
  };

  const handleCheckboxChange = (option: string) => {
    setSelectedOptions((prev) =>
      prev.includes(option) ? prev.filter((item) => item !== option) : [...prev, option]
    );
  };

  const handleSubmit = async () => {
    if (!formData.name || !isValidEmail(formData.email) || !formData.company) return;
    
    const portalId = process.env.NEXT_PUBLIC_HUBSPOT_PORTAL_ID;
    const formId = process.env.NEXT_PUBLIC_HUBSPOT_FORM_ID;

    if (!portalId || !formId) {
      console.error("Error: Missing HubSpot portal or form ID.");
      return;
    }

    const url = `https://api.hsforms.com/submissions/v3/integration/submit/${portalId}/${formId}`;
    const selectedMessage = selectedOptions.join(", ");

    const data = {
      fields: [
        { name: "firstname", value: formData.name },
        { name: "email", value: formData.email },
        { name: "company", value: formData.company },
        { name: "message", value: selectedMessage }
      ],
      context: {
        pageUri: window.location.href,
        pageName: document.title
      }
    };

    try {
      const response = await fetch(url, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Accept: "application/json"
        },
        body: JSON.stringify(data)
      });

      const responseData = await response.json();

      if (response.ok) {
        setSubmitted(true);
        setTimeout(() => {
          setIsOpen(false);
          resetForm();
        }, 1500);
      } else {
        console.error("HubSpot Submission Error:", responseData);
      }
    } catch (error) {
      console.error("Network Error:", error);
    }
  };

  return (
    <Sheet
      open={isOpen}
      onOpenChange={(open) => {
        setIsOpen(open);
        if (!open) resetForm();
      }}
    >
      <SheetContent className="w-full max-w-md p-6">
        {submitted ? (
          <div className="flex flex-col items-center justify-center h-full">
            <CheckCircleIcon className="w-20 h-20 text-green-500" />
            <p className="mt-4 text-lg font-semibold">Thank you, you&apos;re all set.</p>
            <p className="text-gray-500">We’ll be in touch shortly!</p>
          </div>
        ) : (
          <>
            <SheetHeader className="flex flex-col items-center mt-20">
              <Image src={Logo} alt="FalkorDB Logo" width={150} height={40} className="mb-4" />
              <SheetTitle className="text-center text-[1.45rem] font-bold">Let&apos;s talk about your use case</SheetTitle>
              <SheetDescription className="text-center text-lg">Get a follow-up from FalkorDB</SheetDescription>
            </SheetHeader>
            {step === 1 ? (
              <div className="mt-10">
                <h2 className="text-sm text-gray-500">STEP 1 OF 2</h2>
                <p className="mt-2 text-[1.25rem] font-bold">What are you working on?</p>
                <div className="mt-6 space-y-2">
                  {[
                    "Already doing RAG",
                    "Wants to start using RAG",
                    "Want to start using GraphRAG",
                    "Interested in CodeGraph (Code Analysis)"
                  ].map((option) => (
                    <div key={option} className="flex items-center gap-2">
                      <Checkbox
                        checked={selectedOptions.includes(option)}
                        onCheckedChange={() => handleCheckboxChange(option)}
                      />
                      <span>{option}</span>
                    </div>
                  ))}
                </div>
                <Button className="mt-6 w-full bg-black text-white py-2" onClick={() => setStep(2)} disabled={selectedOptions.length === 0}>
                  Next
                </Button>
                <p className="mt-4 text-xs text-gray-500 text-center">
                    I agree that my submitted data is being collected and stored. <strong>We don&apos;t resell your data.</strong>
                </p>
              </div>
            ) : (
              <div className="mt-6">
                <h2 className="text-sm text-gray-500">STEP 2 OF 2</h2>
                <p className="mt-2 text-lg font-bold">Let&apos;s get acquainted</p>
                <div className="mt-4 space-y-4">
                  <Input
                    placeholder="Your Name"
                    value={formData.name}
                    onChange={(e) => setFormData((prev) => ({ ...prev, name: e.target.value }))}
                  />
                  <Input
                    placeholder="Email"
                    type="email"
                    value={formData.email}
                    onChange={(e) => setFormData((prev) => ({ ...prev, email: e.target.value }))}
                  />
                  <Input
                    placeholder="Company Name"
                    value={formData.company}
                    onChange={(e) => setFormData((prev) => ({ ...prev, company: e.target.value }))}
                  />
                </div>
                <div className="mt-6 flex justify-between">
                  <Button variant="secondary" onClick={() => setStep(1)}>
                    Back
                  </Button>
                  <Button className="bg-green-500 text-white py-2" onClick={handleSubmit} disabled={!formData.name || !isValidEmail(formData.email) || !formData.company}>
                    Submit
                  </Button>
                </div>
                <p className="mt-4 text-xs text-gray-500 text-center">
                    I agree that my submitted data is being collected and stored. <strong>We don&apos;t resell your data.</strong>
                </p>
              </div>
            )}
          </>
        )}
      </SheetContent>
    </Sheet>
  );
}
