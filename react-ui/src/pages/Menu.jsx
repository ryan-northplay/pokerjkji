import React from "react";
import { useNavigate } from "react-router-dom";
import MenuButton from "../components/button/MenuButton";
import MenuBody from "../components/layout/MenuBody";

class Menu extends React.Component {
    render() {
        return (
            <MenuBody>
                <p className="text-3xl text-gray-200 font-bold mb-16">
                    Main Menu
                </p>
                <div>
                    <MenuButton className="mb-10" onClick={() => { this.props.navigate("/join"); }} >
                        Join Table
                    </MenuButton>
                </div>
                <div>
                    <MenuButton className="mb-10" onClick={() => { this.props.navigate("/lobby"); }} >
                        Public Tables
                    </MenuButton>
                </div>
                <div>
                    <MenuButton className="mb-10" onClick={() => { this.props.navigate("/create"); }} >
                        Create Table
                    </MenuButton>
                </div>
                <div>
                    <MenuButton className="mb-10" onClick={() => { this.props.navigate("/"); }} >
                        Change Name
                    </MenuButton>
                </div>
            </MenuBody>
        );
    }
};

// Wrap and export
/* eslint import/no-anonymous-default-export: [2, {"allowArrowFunction": true}] */
export default (props) => {
    const navigate = useNavigate();

    return <Menu {...props} navigate={navigate} />;
}

